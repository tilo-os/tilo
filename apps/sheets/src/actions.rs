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
//! the formula-bar edit buffer until Enter. External file selection and bytes
//! are effects; the parsed import is then one bounded `ImportSheets` action.
//! Export and formula recomputation remain derived effects, not stored state.
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

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use tilo_actions::{
    Action, ActionAddress, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, Resource,
};
use ui::tilo_resource::{self, Effect};

use crate::doc::SheetsDoc;
use crate::doc::{ChartKind, ChartSnapshot, CommentReply, CommentThread};
use crate::model::{Addr, CellFormat, GridEdit, a1_to_addr, col_to_letters, letters_to_col};
use crate::settings::DocSettings;
use tilo_locale::{Currency, DateStyle, TimeStyle};

/// Registry `type_id` for every Sheets action.
pub const SHEETS_ACTION_TYPE: &str = "sheets.action";

/// What Sheets does that changes no workbook: reading spreadsheet files into
/// new sheets and writing the workbook out as one, and moving around the grid
/// (views). An import reaches the workbook as one [`SheetsAction::ImportSheets`].
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "sheets.effect")]
pub enum SheetsEffect {
    /// Import CSV
    ///
    /// Adds a CSV file as a new sheet.
    #[effect(job, cancellable, device_only)]
    ImportCsv {
        #[param("Path of the file; omit to choose one")]
        path: Option<String>,
    },
    /// Import Excel workbook
    ///
    /// Adds every sheet of an `.xlsx` workbook.
    #[effect(job, cancellable, device_only)]
    ImportXlsx {
        #[param("Path of the file; omit to choose one")]
        path: Option<String>,
    },
    /// Adds every sheet of an `.ods` spreadsheet.
    #[effect(
        job,
        cancellable,
        device_only,
        label = "Import OpenDocument spreadsheet"
    )]
    ImportOds {
        #[param("Path of the file; omit to choose one")]
        path: Option<String>,
    },
    /// Export CSV
    ///
    /// Saves what the active sheet shows as a CSV file.
    #[effect(job, cancellable, destructive, device_only)]
    ExportCsv {
        #[param("Where to write it; omit to choose")]
        path: Option<String>,
    },
    /// Export Excel workbook
    ///
    /// Saves every sheet as an `.xlsx` workbook.
    #[effect(job, cancellable, destructive, device_only)]
    ExportXlsx {
        #[param("Where to write it; omit to choose")]
        path: Option<String>,
    },
    /// Saves every sheet as an `.ods` spreadsheet.
    #[effect(
        job,
        cancellable,
        destructive,
        device_only,
        label = "Export OpenDocument spreadsheet"
    )]
    ExportOds {
        #[param("Where to write it; omit to choose")]
        path: Option<String>,
    },
    /// Export Excel workbook as content
    ///
    /// Keeps every sheet as an `.xlsx` workbook in the content store, with no
    /// file to write: the caller reads the bytes by the content address it
    /// answers.
    #[effect(content, permission = "read")]
    ExportXlsxContent,
    /// Open command palette
    #[effect(view)]
    Commands,
    /// Go to cell…
    #[effect(view)]
    GoToCell {
        #[param("Cell reference, e.g. B12")]
        reference: String,
    },
    /// Toggle formula view
    ///
    /// Shows the formulas in the grid instead of what they calculate, or back.
    #[effect(view)]
    ToggleFormulaView,
    /// Open
    ///
    /// Shows the sheet with the cell selected.
    #[effect(view, row)]
    Open {
        #[param("The sheet's id")]
        sheet: String,
        #[param("Zero-based column")]
        col: u32,
        #[param("Zero-based row")]
        row: u32,
    },
}

/// What opening a sheet, a cell, a chart or a comment shows: its sheet with
/// that cell selected (the sheet's first cell for the sheet itself).
pub fn open_spec(sheet_id: &str, col: u32, row: u32) -> ActionSpec {
    SheetsEffect::Open {
        sheet: sheet_id.to_owned(),
        col,
        row,
    }
    .bound()
}

pub fn sheet_resource_id(sheet_id: &str) -> String {
    format!("sheets.sheet:{sheet_id}")
}

pub fn cell_resource_id(sheet_id: &str, col: u32, row: u32) -> String {
    format!("sheets.cell:{sheet_id}:{col}:{row}")
}

pub fn chart_resource_id(sheet_id: &str, chart_id: &str) -> String {
    format!("sheets.chart:{sheet_id}:{chart_id}")
}

pub fn comment_resource_id(sheet_id: &str, comment_id: &str) -> String {
    format!("sheets.comment:{sheet_id}:{comment_id}")
}

pub fn open_sheet_address(sheet_id: &str) -> ActionAddress {
    ActionAddress::new(sheet_resource_id(sheet_id), "open")
}

pub fn open_cell_address(sheet_id: &str, col: u32, row: u32) -> ActionAddress {
    ActionAddress::new(cell_resource_id(sheet_id, col, row), "open")
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
/// Imports deliberately share the same bounded action transport as a paste.
/// Keeping the cap here (rather than only in the parser) protects remote
/// replay and history from a malicious or stale import envelope too.
const MAX_IMPORTED_SHEETS: usize = 64;
/// A whole workbook import has its own bound: it is one atomic action rather
/// than a normal paste, but still has a strict replay/history budget.
pub const MAX_IMPORTED_CELLS: usize = 64 * 1024;
const MAX_IMPORT_RAW_BYTES: usize = 8 * 1024 * 1024;
const MAX_IMPORT_SOURCE_BYTES: usize = 4 * 1024;
const MAX_SHEET_NAME_CHARS: usize = 100;
const TAB_COLOR_COUNT: u32 = 6;
const MAX_ANNOTATION_BYTES: usize = 4 * 1024;

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

/// One newly-created sheet in an atomic imported workbook action.  It carries
/// an author-created identity so a replay never falls back to a positional
/// sheet target after peers reorder their tabs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedSheet {
    pub name: String,
    pub sheet_id: String,
    pub cells: Vec<CellWrite>,
}

/// Every collaborative operation Sheets exports on the shared workbook.
///
/// Determinism contract: coordinates and values are carried in the payload;
/// [`Action::apply`] never touches the RNG or the wall-clock and emits its ops
/// in a stable order. A malformed or stale payload is refused atomically rather
/// than partially applying its valid prefix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SheetsAction {
    /// Atomically add one or more imported sheets and their cells.  Parsing a
    /// local file is an external effect, but the resulting workbook content is
    /// a normal collaborative action: one undo point, one history entry, and
    /// a byte-identical peer replay with no privately-written half import.
    ImportSheets {
        sheets: Vec<ImportedSheet>,
        /// Drop the workbook's only sheet when it is still blank, so a file
        /// imported into a new workbook is the workbook rather than its
        /// second tab.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        replace_blank: bool,
        /// The file the sheets were read from, recorded on each so opening
        /// that file again reveals them instead of importing a copy.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
    },
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
    /// Style every cell of the inclusive range `c0,r0:c1,r1`: bold text and/or
    /// a display format. `None` leaves that attribute as it is.
    SetStyle {
        sheet_id: String,
        c0: u32,
        r0: u32,
        c1: u32,
        r1: u32,
        bold: Option<bool>,
        fmt: Option<String>,
    },
    /// Delete several rows (zero-based, as they are before the action) in one
    /// change; the rows below each move up.
    DeleteRows { sheet_id: String, rows: Vec<u32> },
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
    /// Add or replace a durable chart with an exact selected source range.
    PutChart {
        sheet: usize,
        sheet_id: String,
        chart_id: String,
        title: String,
        kind: String,
        c0: u32,
        r0: u32,
        c1: u32,
        r1: u32,
    },
    DeleteChart {
        sheet: usize,
        sheet_id: String,
        chart_id: String,
    },
    /// One threaded annotation lives at a stable cell/range anchor. Its
    /// author is whoever invokes the action.
    PutComment {
        sheet: usize,
        sheet_id: String,
        comment_id: String,
        c0: u32,
        r0: u32,
        c1: u32,
        r1: u32,
        body: String,
    },
    /// A reply to a comment thread, by whoever invokes the action.
    ReplyComment {
        sheet: usize,
        sheet_id: String,
        comment_id: String,
        reply_id: String,
        body: String,
    },
    EditComment {
        sheet: usize,
        sheet_id: String,
        comment_id: String,
        body: String,
    },
    DeleteComment {
        sheet: usize,
        sheet_id: String,
        comment_id: String,
    },
    SetCommentResolved {
        sheet: usize,
        sheet_id: String,
        comment_id: String,
        resolved: bool,
    },
}

/// Durable, browseable history metadata for a workbook mutation.
pub fn history_context(action: &SheetsAction) -> tilo_actions::ActionContext {
    let mut context = tilo_actions::ActionContext::for_action("so.tilo.sheets", "Sheets", action);
    let (label, id, kind, resource_label) = history_resource(action);
    context.action_label = label.to_string();
    context.resource = Some(tilo_actions::ActionResource {
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

#[allow(
    clippy::too_many_lines,
    reason = "history labels are exhaustive over the replayable Sheets action enum and stay adjacent to its action semantics"
)]
fn history_resource(action: &SheetsAction) -> HistoryResource {
    match action {
        SheetsAction::ImportSheets { sheets, .. } => (
            "Import workbook",
            "sheets.workbook".to_string(),
            "sheets.workbook",
            format!("{} imported sheet(s)", sheets.len()),
        ),
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
        SheetsAction::SetStyle {
            sheet_id,
            c0,
            r0,
            c1,
            r1,
            ..
        } => range_resource("Format range", sheet_id, range_area(*c0, *r0, *c1, *r1)),
        SheetsAction::DeleteRows { sheet_id, rows } => (
            "Delete rows",
            format!("sheets.sheet:{sheet_id}"),
            "sheets.sheet",
            format!("{} row(s)", rows.len()),
        ),
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
        SheetsAction::PutChart {
            sheet_id,
            chart_id,
            title,
            ..
        } => (
            "Update chart",
            format!("sheets.chart:{sheet_id}:{chart_id}"),
            "sheets.chart",
            title.clone(),
        ),
        SheetsAction::DeleteChart {
            sheet_id, chart_id, ..
        } => (
            "Delete chart",
            format!("sheets.chart:{sheet_id}:{chart_id}"),
            "sheets.chart",
            "Chart".into(),
        ),
        SheetsAction::PutComment {
            sheet_id,
            comment_id,
            c0,
            r0,
            ..
        } => (
            "Add comment",
            format!("sheets.comment:{sheet_id}:{comment_id}"),
            "sheets.comment",
            format!("{}{}", column_label(*c0), r0 + 1),
        ),
        SheetsAction::ReplyComment {
            sheet_id,
            comment_id,
            ..
        } => (
            "Reply to comment",
            format!("sheets.comment:{sheet_id}:{comment_id}"),
            "sheets.comment",
            "Comment thread".into(),
        ),
        SheetsAction::EditComment {
            sheet_id,
            comment_id,
            ..
        } => (
            "Edit comment",
            format!("sheets.comment:{sheet_id}:{comment_id}"),
            "sheets.comment",
            "Comment thread".into(),
        ),
        SheetsAction::DeleteComment {
            sheet_id,
            comment_id,
            ..
        } => (
            "Delete comment",
            format!("sheets.comment:{sheet_id}:{comment_id}"),
            "sheets.comment",
            "Comment thread".into(),
        ),
        SheetsAction::SetCommentResolved {
            sheet_id,
            comment_id,
            resolved,
            ..
        } => (
            if *resolved {
                "Resolve comment"
            } else {
                "Reopen comment"
            },
            format!("sheets.comment:{sheet_id}:{comment_id}"),
            "sheets.comment",
            "Comment thread".into(),
        ),
    }
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
            SheetsAction::ImportSheets {
                sheets,
                replace_blank,
                source,
            } => {
                let blank = *replace_blank && workbook_is_blank(doc);
                for imported in sheets {
                    let sheet =
                        SheetsDoc::add_sheet_with_id(doc, &imported.name, &imported.sheet_id);
                    if let Some(source) = source {
                        SheetsDoc::set_sheet_source(doc, sheet, source);
                    }
                    apply_set_cells(doc, sheet, &imported.cells);
                }
                if blank && !sheets.is_empty() {
                    SheetsDoc::delete_sheet(doc, 0);
                }
            }
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
            SheetsAction::SetStyle {
                sheet_id,
                c0,
                r0,
                c1,
                r1,
                bold,
                fmt,
            } => {
                let sheet = resolve_sheet(doc, sheet_id)?;
                for row in *r0..=*r1 {
                    for col in *c0..=*c1 {
                        if let Some(bold) = bold {
                            SheetsDoc::set_cell_bold(doc, sheet, (col, row), *bold);
                        }
                        if let Some(fmt) = fmt {
                            SheetsDoc::set_cell_format(
                                doc,
                                sheet,
                                (col, row),
                                CellFormat::from_str(fmt),
                            );
                        }
                    }
                }
            }
            SheetsAction::DeleteRows { sheet_id, rows } => {
                let sheet = resolve_sheet(doc, sheet_id)?;
                let mut rows = rows.clone();
                rows.sort_unstable();
                rows.dedup();
                for at in rows.into_iter().rev() {
                    restructure(doc, sheet, GridEdit::DeleteRow(at));
                }
            }
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
                restructure(doc, resolve_sheet(doc, sheet_id)?, GridEdit::InsertRow(*at));
            }
            SheetsAction::DeleteRow { sheet_id, at, .. } => {
                restructure(doc, resolve_sheet(doc, sheet_id)?, GridEdit::DeleteRow(*at));
            }
            SheetsAction::InsertColumn { sheet_id, at, .. } => {
                restructure(
                    doc,
                    resolve_sheet(doc, sheet_id)?,
                    GridEdit::InsertColumn(*at),
                );
            }
            SheetsAction::DeleteColumn { sheet_id, at, .. } => {
                restructure(
                    doc,
                    resolve_sheet(doc, sheet_id)?,
                    GridEdit::DeleteColumn(*at),
                );
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
            SheetsAction::PutChart {
                sheet_id,
                chart_id,
                title,
                kind,
                c0,
                r0,
                c1,
                r1,
                ..
            } => {
                SheetsDoc::put_chart(
                    doc,
                    resolve_sheet(doc, sheet_id)?,
                    &ChartSnapshot {
                        id: chart_id.clone(),
                        title: title.clone(),
                        kind: ChartKind::parse(kind).unwrap_or(ChartKind::Column),
                        start: (*c0, *r0),
                        end: (*c1, *r1),
                    },
                );
            }
            SheetsAction::DeleteChart {
                sheet_id, chart_id, ..
            } => SheetsDoc::delete_chart(doc, resolve_sheet(doc, sheet_id)?, chart_id),
            SheetsAction::PutComment {
                sheet_id,
                comment_id,
                c0,
                r0,
                c1,
                r1,
                body,
                ..
            } => {
                let author = invoking_author(doc);
                SheetsDoc::put_comment(
                    doc,
                    resolve_sheet(doc, sheet_id)?,
                    &CommentThread {
                        id: comment_id.clone(),
                        start: (*c0, *r0),
                        end: (*c1, *r1),
                        author,
                        body: body.clone(),
                        resolved: false,
                        replies: Vec::new(),
                    },
                );
            }
            SheetsAction::ReplyComment {
                sheet_id,
                comment_id,
                reply_id,
                body,
                ..
            } => {
                let author = invoking_author(doc);
                let sheet = resolve_sheet(doc, sheet_id)?;
                if SheetsDoc::comments(doc, sheet)
                    .iter()
                    .any(|thread| thread.id == *comment_id)
                {
                    SheetsDoc::put_comment_reply(
                        doc,
                        sheet,
                        comment_id,
                        &CommentReply {
                            id: reply_id.clone(),
                            author,
                            body: body.clone(),
                        },
                    );
                } else {
                    return refuse("comment thread no longer exists");
                }
            }
            SheetsAction::SetCommentResolved {
                sheet_id,
                comment_id,
                resolved,
                ..
            } => {
                let sheet = resolve_sheet(doc, sheet_id)?;
                if !SheetsDoc::comments(doc, sheet)
                    .iter()
                    .any(|thread| thread.id == *comment_id)
                {
                    return refuse("comment thread no longer exists");
                }
                SheetsDoc::set_comment_resolved(doc, sheet, comment_id, *resolved);
            }
            SheetsAction::EditComment {
                sheet_id,
                comment_id,
                body,
                ..
            } => {
                let sheet = resolve_sheet(doc, sheet_id)?;
                if !SheetsDoc::comments(doc, sheet)
                    .iter()
                    .any(|thread| thread.id == *comment_id)
                {
                    return refuse("comment thread no longer exists");
                }
                SheetsDoc::edit_comment_body(doc, sheet, comment_id, body);
            }
            SheetsAction::DeleteComment {
                sheet_id,
                comment_id,
                ..
            } => {
                let sheet = resolve_sheet(doc, sheet_id)?;
                if !SheetsDoc::comments(doc, sheet)
                    .iter()
                    .any(|thread| thread.id == *comment_id)
                {
                    return refuse("comment thread no longer exists");
                }
                SheetsDoc::delete_comment(doc, sheet, comment_id);
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

/// Who invoked the action applying now: the author a comment is signed
/// with, pinned with the action so every replica signs it alike. Empty for
/// an edit written with no invoker.
fn invoking_author(doc: &Doc) -> String {
    ui::tilo_resource::ActionCx::for_write(doc, &[])
        .author()
        .unwrap_or_default()
        .to_owned()
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
    let ids: Vec<String> = (0..SheetsDoc::sheet_count(doc))
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
    let ids: Vec<String> = (0..SheetsDoc::sheet_count(doc))
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

/// Validate an import envelope completely before creating its first sheet.
/// This is deliberately separate from `validate_writes`: imported sheets do
/// not exist yet, while their coordinates/formats need exactly the same
/// hostile-input checks as a normal paste.
fn validate_imported_sheets(doc: &Doc, sheets: &[ImportedSheet]) -> Result<(), ActionError> {
    if sheets.is_empty() || sheets.len() > MAX_IMPORTED_SHEETS {
        return refuse("an import must contain between one and 64 sheets");
    }
    let mut ids = HashSet::new();
    let mut total_cells = 0usize;
    let mut total_raw_bytes = 0usize;
    for sheet in sheets {
        if !valid_sheet_name(&sheet.name) {
            return refuse(
                "imported sheet name is empty, too long, or contains control characters",
            );
        }
        validate_new_sheet_id(doc, &sheet.sheet_id)?;
        if !ids.insert(sheet.sheet_id.as_str()) {
            return refuse("an import reuses a sheet identity");
        }
        let mut seen = HashSet::new();
        for cell in &sheet.cells {
            total_cells = total_cells.saturating_add(1);
            total_raw_bytes = total_raw_bytes.saturating_add(cell.raw.len());
            if total_cells > MAX_IMPORTED_CELLS {
                return refuse("an import has too many populated cells");
            }
            if total_raw_bytes > MAX_IMPORT_RAW_BYTES {
                return refuse("imported cell input is too large");
            }
            if !valid_addr(cell.col, cell.row) || cell.raw.len() > MAX_CELL_INPUT_BYTES {
                return refuse("imported cell is outside the workbook bounds");
            }
            if cell.fmt.as_deref().is_some_and(|fmt| !valid_format(fmt)) {
                return refuse("imported cell format is invalid");
            }
            if !seen.insert((cell.col, cell.row)) {
                return refuse("an imported sheet contains the same cell more than once");
            }
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
        SheetsAction::ImportSheets { sheets, source, .. } => {
            if source
                .as_deref()
                .is_some_and(|source| source.is_empty() || source.len() > MAX_IMPORT_SOURCE_BYTES)
            {
                return refuse("an import's source path is empty or too long");
            }
            validate_imported_sheets(doc, sheets)
        }
        SheetsAction::SetStyle {
            sheet_id,
            c0,
            r0,
            c1,
            r1,
            bold: _,
            fmt,
        } => {
            validate_sheet(doc, 0, sheet_id)?;
            if !valid_range(*c0, *r0, *c1, *r1)
                || range_area(*c0, *r0, *c1, *r1) > MAX_WRITES_PER_ACTION
                || fmt.as_deref().is_some_and(|fmt| !valid_format(fmt))
            {
                return refuse("style range or format is invalid");
            }
            Ok(())
        }
        SheetsAction::DeleteRows { sheet_id, rows } => {
            validate_sheet(doc, 0, sheet_id)?;
            if rows.is_empty()
                || rows.len() > MAX_WRITES_PER_ACTION
                || rows.iter().any(|row| *row > MAX_ROW)
            {
                return refuse("rows to delete are invalid");
            }
            Ok(())
        }
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
        SheetsAction::PutChart {
            sheet,
            sheet_id,
            chart_id,
            title,
            kind,
            c0,
            r0,
            c1,
            r1,
        } => {
            validate_sheet(doc, *sheet, sheet_id)?;
            if Uuid::parse_str(chart_id).is_err()
                || title.trim().is_empty()
                || title.len() > MAX_ANNOTATION_BYTES
                || ChartKind::parse(kind).is_none()
                || !valid_range(*c0, *r0, *c1, *r1)
            {
                return refuse("chart payload is invalid");
            }
            Ok(())
        }
        SheetsAction::DeleteChart {
            sheet,
            sheet_id,
            chart_id,
        } => {
            validate_sheet(doc, *sheet, sheet_id)?;
            if Uuid::parse_str(chart_id).is_ok() {
                Ok(())
            } else {
                refuse("chart identity is invalid")
            }
        }
        SheetsAction::PutComment {
            sheet,
            sheet_id,
            comment_id,
            c0,
            r0,
            c1,
            r1,
            body,
        } => {
            validate_sheet(doc, *sheet, sheet_id)?;
            if Uuid::parse_str(comment_id).is_err()
                || !valid_range(*c0, *r0, *c1, *r1)
                || !valid_annotation(body)
            {
                return refuse("comment payload is invalid");
            }
            Ok(())
        }
        SheetsAction::ReplyComment {
            sheet,
            sheet_id,
            comment_id,
            reply_id,
            body,
        } => {
            validate_sheet(doc, *sheet, sheet_id)?;
            if Uuid::parse_str(comment_id).is_err()
                || Uuid::parse_str(reply_id).is_err()
                || !valid_annotation(body)
            {
                return refuse("comment reply payload is invalid");
            }
            Ok(())
        }
        SheetsAction::SetCommentResolved {
            sheet,
            sheet_id,
            comment_id,
            ..
        }
        | SheetsAction::DeleteComment {
            sheet,
            sheet_id,
            comment_id,
        } => {
            validate_sheet(doc, *sheet, sheet_id)?;
            if Uuid::parse_str(comment_id).is_ok() {
                Ok(())
            } else {
                refuse("comment identity is invalid")
            }
        }
        SheetsAction::EditComment {
            sheet,
            sheet_id,
            comment_id,
            body,
        } => {
            validate_sheet(doc, *sheet, sheet_id)?;
            if Uuid::parse_str(comment_id).is_ok() && valid_annotation(body) {
                Ok(())
            } else {
                refuse("comment edit payload is invalid")
            }
        }
    }
}

fn valid_range(c0: u32, r0: u32, c1: u32, r1: u32) -> bool {
    valid_addr(c0, r0) && valid_addr(c1, r1) && c0 <= c1 && r0 <= r1
}

fn range_area(c0: u32, r0: u32, c1: u32, r1: u32) -> usize {
    let width = usize::try_from(c1.saturating_sub(c0))
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    let height = usize::try_from(r1.saturating_sub(r0))
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    width.saturating_mul(height)
}

/// A workbook of one sheet with no cells, formats, charts or comments.
fn workbook_is_blank(doc: &Doc) -> bool {
    if SheetsDoc::sheet_count(doc) != 1 {
        return false;
    }
    let snap = SheetsDoc::snapshot_sheet(doc, 0, APPLY_CURRENCY);
    snap.raw.is_empty()
        && snap.fmt.is_empty()
        && snap.bold.is_empty()
        && SheetsDoc::charts(doc, 0).is_empty()
        && SheetsDoc::comments(doc, 0).is_empty()
}

fn valid_annotation(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_ANNOTATION_BYTES
        && !value.chars().any(char::is_control)
}

/// Rebuild `sheet`'s populated cells through `edit`: each cell moves to
/// `edit.map_addr(addr)`, or is dropped when its row/column is deleted. The document's cells
/// are read at the replay's base state and every op is emitted in a **sorted**
/// order so two replicas produce the byte-identical change (a `HashMap`'s
/// iteration order is not stable across replicas). Old cells are cleared before
/// the new ones are written so an overlapping shift is lossless.
fn restructure(doc: &mut Doc, sheet: usize, edit: GridEdit) {
    let map = |addr| edit.map_addr(addr);
    let snap = SheetsDoc::snapshot_sheet(doc, sheet, APPLY_CURRENCY);

    let mut new_raw: HashMap<Addr, String> = HashMap::new();
    let mut new_fmt: HashMap<Addr, CellFormat> = HashMap::new();
    for (addr, raw) in &snap.raw {
        if let Some(dst) = map(*addr) {
            // Structural changes move cell *relationships*, not merely the
            // formula-owning cell. `$` anchors only govern copy/fill, so row
            // and column inserts remap both anchored and relative references.
            new_raw.insert(dst, crate::formula::remap_refs(raw, edit));
        }
    }
    for (addr, fmt) in &snap.fmt {
        if let Some(dst) = map(*addr) {
            new_fmt.insert(dst, *fmt);
        }
    }
    let mut new_bold: Vec<Addr> = snap.bold.iter().filter_map(|addr| map(*addr)).collect();
    new_bold.sort_unstable();

    // Clear every previously-populated cell first, in a stable order.
    let mut old: Vec<Addr> = snap
        .raw
        .keys()
        .chain(snap.fmt.keys())
        .chain(snap.bold.iter())
        .copied()
        .collect();
    old.sort_unstable();
    old.dedup();
    for addr in old {
        SheetsDoc::set_cell_raw(doc, sheet, addr, "");
        SheetsDoc::set_cell_format(doc, sheet, addr, CellFormat::General);
        SheetsDoc::set_cell_bold(doc, sheet, addr, false);
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
    for addr in new_bold {
        SheetsDoc::set_cell_bold(doc, sheet, addr, true);
    }
    // Charts and discussion anchors describe the same coordinate system as
    // cells, so structural edits must move them in this exact replay action.
    SheetsDoc::remap_annotation_anchors(doc, sheet, edit);
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

/// Build the one replayable action produced after an external spreadsheet has
/// been parsed and fully preflighted.  The caller supplies rows in stable
/// order; each new sheet gets a non-reused resource identity at author time.
pub fn import_sheets_action(
    sheets: impl IntoIterator<Item = (String, Vec<CellWrite>)>,
) -> SheetsAction {
    SheetsAction::ImportSheets {
        sheets: sheets
            .into_iter()
            .map(|(name, cells)| ImportedSheet {
                name,
                sheet_id: Uuid::new_v4().to_string(),
                cells,
            })
            .collect(),
        replace_blank: false,
        source: None,
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

pub fn put_chart_action(
    sheet: usize,
    sheet_id: impl Into<String>,
    title: &str,
    kind: ChartKind,
    start: Addr,
    end: Addr,
) -> SheetsAction {
    SheetsAction::PutChart {
        sheet,
        sheet_id: sheet_id.into(),
        chart_id: Uuid::new_v4().to_string(),
        title: title.to_string(),
        kind: kind.code().to_string(),
        c0: start.0,
        r0: start.1,
        c1: end.0,
        r1: end.1,
    }
}

pub fn put_comment_action(
    sheet: usize,
    sheet_id: impl Into<String>,
    start: Addr,
    end: Addr,
    body: &str,
) -> SheetsAction {
    SheetsAction::PutComment {
        sheet,
        sheet_id: sheet_id.into(),
        comment_id: Uuid::new_v4().to_string(),
        c0: start.0,
        r0: start.1,
        c1: end.0,
        r1: end.1,
        body: body.to_string(),
    }
}

pub fn reply_comment_action(
    sheet: usize,
    sheet_id: impl Into<String>,
    comment_id: impl Into<String>,
    body: &str,
) -> SheetsAction {
    SheetsAction::ReplyComment {
        sheet,
        sheet_id: sheet_id.into(),
        comment_id: comment_id.into(),
        reply_id: Uuid::new_v4().to_string(),
        body: body.to_string(),
    }
}

pub fn edit_comment_action(
    sheet: usize,
    sheet_id: impl Into<String>,
    comment_id: impl Into<String>,
    body: &str,
) -> SheetsAction {
    SheetsAction::EditComment {
        sheet,
        sheet_id: sheet_id.into(),
        comment_id: comment_id.into(),
        body: body.to_string(),
    }
}

pub fn set_comment_resolved_action(
    sheet: usize,
    sheet_id: impl Into<String>,
    comment_id: impl Into<String>,
    resolved: bool,
) -> SheetsAction {
    SheetsAction::SetCommentResolved {
        sheet,
        sheet_id: sheet_id.into(),
        comment_id: comment_id.into(),
        resolved,
    }
}

pub fn delete_comment_action(
    sheet: usize,
    sheet_id: impl Into<String>,
    comment_id: impl Into<String>,
) -> SheetsAction {
    SheetsAction::DeleteComment {
        sheet,
        sheet_id: sheet_id.into(),
        comment_id: comment_id.into(),
    }
}

/// Parse the accessible chart editor's compact, copyable current-value form:
/// `Title | column | B2:E9`. Keeping the parser beside the action builders
/// ensures agents, Ctrl+K and future native editors all author the exact same
/// stable-ID upsert payload.
pub fn edit_chart_form_action(
    sheet: usize,
    sheet_id: impl Into<String>,
    chart_id: impl Into<String>,
    form: &str,
) -> Option<SheetsAction> {
    let mut parts = form.split('|').map(str::trim);
    let (Some(title), Some(kind), Some(range), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let (start, end) = range.split_once(':').map_or_else(
        || {
            let addr = a1_to_addr(range)?;
            Some((addr, addr))
        },
        |(start, end)| Some((a1_to_addr(start)?, a1_to_addr(end)?)),
    )?;
    let kind = ChartKind::parse(&kind.to_ascii_lowercase())?;
    Some(SheetsAction::PutChart {
        sheet,
        sheet_id: sheet_id.into(),
        chart_id: chart_id.into(),
        title: title.to_string(),
        kind: kind.code().to_string(),
        c0: start.0.min(end.0),
        r0: start.1.min(end.1),
        c1: start.0.max(end.0),
        r1: start.1.max(end.1),
    })
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

/// A populated cell with what it shows, for search and sorting.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchableCell {
    pub col: u32,
    pub row: u32,
    pub raw: String,
    pub display: String,
    /// The computed value, which sorting orders by (a formula's result, a
    /// currency amount as its number rather than its `$1,200.00`).
    pub value: crate::model::CellValue,
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

/// Sheet / chart / comment children for [`tilo_resource::publish`].
#[allow(
    clippy::too_many_lines,
    clippy::missing_panics_doc,
    clippy::expect_used,
    reason = "resource surface is deliberately co-located and serializes owned enum values"
)]
pub(crate) fn document_children(state: &crate::resource::SheetsState) -> Vec<Resource> {
    state
        .sheets
        .iter()
        .enumerate()
        .filter_map(|(idx, sheet)| {
            if sheet.id.is_empty() {
                return None;
            }
            Some(sheet_document_resource(idx, sheet))
        })
        .collect()
}

#[allow(
    clippy::too_many_lines,
    reason = "sheet document surface still lists every cell/view action inline"
)]
fn sheet_document_resource(
    idx: usize,
    sheet: &std::sync::Arc<crate::resource::SheetInfo>,
) -> Resource {
    let name = &sheet.name;
    let label = if name.trim().is_empty() {
        format!("Sheet{}", idx + 1)
    } else {
        name.clone()
    };
    // Never publish a positional fallback: after a concurrent reorder it
    // could make an old address operate on a different sheet. Legacy or
    // malformed rows without a durable identity remain usable on-screen,
    // but are deliberately absent from the headless action surface.
    let stable = sheet.id.clone();
    let mut resource = Resource::new(sheet_resource_id(&stable), "sheets.sheet", label)
        .with_action(open_spec(&stable, 0, 0));

    // Rename — a prompted action carrying the new name.
    resource = resource.with_action(ActionSpec::prompted(
        "rename",
        "Rename sheet",
        "Name",
        "sheet name…",
        ExecutionLocus::ClientLocal,
        {
            let stable = stable.clone();
            move |new_name: &str| encode(&rename_sheet_action(idx, stable.clone(), new_name.trim()))
        },
    ));

    // Delete the whole sheet (a final sheet is deliberately a no-op).
    resource = resource.with_action(immediate_spec(
        "delete",
        "Delete sheet",
        &delete_sheet_action(idx, stable.clone()),
    ));

    // Insert a blank row above a given 1-based row number (default: the top).
    resource = resource.with_action(ActionSpec::prompted(
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
    resource = resource.with_action(ActionSpec::prompted(
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

    for spec in crate::cell_actions::sheet_actions(idx, &stable, std::sync::Arc::clone(sheet)) {
        resource = resource.with_action(spec);
    }

    // Charts and threads are canonical children, not popup-only state.
    // Each carries its UUID resource identity through deletion/resolution.
    for chart in &sheet.charts {
        let chart_id = chart.id.clone();
        let current_form = format!(
            "{} | {} | {}:{}",
            chart.title,
            chart.kind.code(),
            crate::model::addr_to_a1(chart.start.0, chart.start.1),
            crate::model::addr_to_a1(chart.end.0, chart.end.1),
        );
        let chart_resource = Resource::new(
            chart_resource_id(&stable, &chart_id),
            "sheets.chart",
            chart.title.clone(),
        )
        .with_action(open_spec(&stable, chart.start.0, chart.start.1))
        .with_action(ActionSpec::prompted(
            "edit",
            "Edit chart",
            "Title | type | source range",
            format!("Current: {current_form}"),
            ExecutionLocus::ClientLocal,
            {
                let stable = stable.clone();
                let chart_id = chart_id.clone();
                move |form: &str| {
                    edit_chart_form_action(idx, stable.clone(), chart_id.clone(), form).map_or_else(
                        || {
                            encode(&SheetsAction::PutChart {
                                sheet: idx,
                                sheet_id: stable.clone(),
                                chart_id: chart_id.clone(),
                                title: String::new(),
                                kind: "invalid".into(),
                                c0: 1,
                                r0: 1,
                                c1: 0,
                                r1: 0,
                            })
                        },
                        |action| encode(&action),
                    )
                }
            },
        ))
        .with_action(immediate_spec(
            "delete",
            "Delete chart",
            &SheetsAction::DeleteChart {
                sheet: idx,
                sheet_id: stable.clone(),
                chart_id,
            },
        ));
        resource = resource.with_child(chart_resource);
    }
    for thread in &sheet.comments {
        let comment_id = thread.id.clone();
        let action =
            set_comment_resolved_action(idx, stable.clone(), comment_id.clone(), !thread.resolved);
        let label = if thread.resolved {
            "Reopen comment"
        } else {
            "Resolve comment"
        };
        let comment = Resource::new(
            comment_resource_id(&stable, &comment_id),
            "sheets.comment",
            format!("{}{}", column_label(thread.start.0), thread.start.1 + 1),
        )
        .with_action(open_spec(&stable, thread.start.0, thread.start.1))
        .with_action(
            ActionSpec::prompted(
                "edit",
                "Edit comment",
                "Comment",
                "comment…",
                ExecutionLocus::ClientLocal,
                {
                    let stable = stable.clone();
                    let comment_id = comment_id.clone();
                    move |body: &str| {
                        encode(&edit_comment_action(
                            idx,
                            stable.clone(),
                            comment_id.clone(),
                            body.trim(),
                        ))
                    }
                },
            )
            .requiring(tilo_actions::PermissionRequirement::Comment),
        )
        .with_action(
            ActionSpec::prompted(
                "reply",
                "Reply to comment",
                "Reply",
                "reply…",
                ExecutionLocus::ClientLocal,
                {
                    let stable = stable.clone();
                    let comment_id = comment_id.clone();
                    move |body: &str| {
                        encode(&reply_comment_action(
                            idx,
                            stable.clone(),
                            comment_id.clone(),
                            body.trim(),
                        ))
                    }
                },
            )
            .requiring(tilo_actions::PermissionRequirement::Comment),
        )
        .with_action(
            immediate_spec("toggle-resolved", label, &action)
                .requiring(tilo_actions::PermissionRequirement::Comment),
        )
        .with_action(immediate_spec(
            "delete",
            "Delete comment",
            &delete_comment_action(idx, stable.clone(), comment_id),
        ));
        resource = resource.with_child(comment);
    }

    resource
}

/// The workbook as the framework publishes it for `state`.
pub fn published(state: &crate::resource::SheetsState) -> Resource {
    ui::tilo_resource::publish::<crate::resource::SheetsWorkbook>(
        <crate::resource::SheetsWorkbook as ui::tilo_resource::Resource>::KIND,
        <crate::resource::SheetsWorkbook as ui::tilo_resource::Resource>::LABEL,
        state,
    )
}

/// The sheet id, column and row a cell's resource id names, when it names
/// one inside the grid.
pub fn parse_cell_resource_id(id: &str) -> Option<(&str, u32, u32)> {
    let mut parts = id.strip_prefix("sheets.cell:")?.split(':');
    let (Some(sheet_id), Some(col), Some(row), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let (Ok(col), Ok(row)) = (col.parse::<u32>(), row.parse::<u32>()) else {
        return None;
    };
    (col <= MAX_COLUMN && row <= MAX_ROW).then_some((sheet_id, col, row))
}

/// One cell, which a search hit or an agent opens.
pub(crate) fn open_cell_resource(sheet_id: &str, col: u32, row: u32) -> Resource {
    let reference = column_label(col) + &(row + 1).to_string();
    Resource::new(
        cell_resource_id(sheet_id, col, row),
        "sheets.cell",
        reference,
    )
    .with_action(open_spec(sheet_id, col, row))
}

/// The 1-based column letters for `col`, used in action labels/status lines.
pub fn column_label(col: u32) -> String {
    col_to_letters(col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CellFormat, CellValue};
    use automerge::AutoCommit;
    use tilo_actions::{ExecutionMode, LocalTransport, ResultSpec};
    use ui::tilo_resource::Resource as _;

    fn registry() -> ActionRegistry {
        ActionRegistry::with(register)
    }

    /// Dispatch an action through the executor, as the on-screen affordance path
    /// does, and return the resulting document heads.
    fn dispatch(doc: &mut AutoCommit, action: &SheetsAction) {
        let encoded = encode(action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
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

    /// Ctrl+K tree, bounded open snapshots, lazy/stale cell addresses and identity across moves.
    #[test]
    fn workbook_tree_and_snapshots_stay_bounded_and_canonical() {
        // Case `ctrl_k_publishes_the_workbook_and_sheet_tree`.
        // Ctrl+K on Sheets yields the workbook root exporting "New sheet" and a
        // `sheets.sheet:<idx>` child per sheet, each exporting rename / delete /
        // insert-row / insert-column. Drives the real resource-builder path.
        {
            let doc = seeded();
            let resources = resources_for(&doc);
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
        }

        // Case `ten_thousand_cells_stay_out_of_the_open_snapshot`.
        {
            let mut doc = AutoCommit::new();
            let sheet = SheetsDoc::add_sheet_with_id(
                &mut doc,
                "Scale",
                "10000000-0000-4000-8000-00000000000a",
            );
            for row in 0..10_000 {
                SheetsDoc::set_cell_raw(&mut doc, sheet, (0, row), &row.to_string());
            }
            let resources = resources_for(&doc);
            assert!(resources[0].children[0].children.is_empty());
            assert_eq!(
                open_cell_resource("10000000-0000-4000-8000-00000000000a", 0, 9_999)
                    .id
                    .0,
                "sheets.cell:10000000-0000-4000-8000-00000000000a:0:9999"
            );
        }

        // Case `omitted_cells_resolve_lazily_but_stale_or_out_of_range_addresses_refuse`.
        {
            use crate::resource::SheetsWorkbook;

            let mut doc = AutoCommit::new();
            crate::sample::write(&mut doc);
            let state = SheetsWorkbook::read(&doc).unwrap();
            let sheet_id = SheetsDoc::sheet_resource_id(&doc, 0).expect("seeded sheet identity");
            let id = cell_resource_id(&sheet_id, 10, 100_000);
            let lazy = SheetsWorkbook::child(&state, &id).expect("valid omitted cell resolves");
            assert_eq!(lazy.id.0, id);
            assert!(
                tilo_actions::door::resolve(&[lazy], &open_cell_address(&sheet_id, 10, 100_000))
                    .is_some()
            );
            assert!(
                SheetsWorkbook::child(&state, &cell_resource_id(&sheet_id, MAX_COLUMN + 1, 0))
                    .is_none()
            );

            dispatch(&mut doc, &delete_sheet_action(0, sheet_id));
            let state = SheetsWorkbook::read(&doc).unwrap();
            assert!(
                SheetsWorkbook::child(&state, &id).is_none(),
                "a stale sheet id has no authority"
            );
        }

        // Case `document_context_and_palette_keep_sheet_identity_across_move`.
        {
            let mut doc = seeded();
            let stable = SheetsDoc::sheet_resource_id(&doc, 0).expect("stable sheet identity");
            let before = history_context(&SheetsAction::SetCell {
                sheet: 0,
                sheet_id: stable.clone(),
                col: 1,
                row: 4,
                raw: "42".into(),
                fmt: None,
            });
            assert_eq!(
                before.resource.expect("cell resource").id,
                format!("sheets.cell:{stable}:1:4")
            );

            SheetsDoc::move_sheet(&mut doc, 0, 1);
            let resources = resources_for(&doc);
            let moved = resources[0]
                .children
                .iter()
                .find(|resource| resource.label == "Budget")
                .expect("moved sheet remains published");
            assert_eq!(moved.id.0, format!("sheets.sheet:{stable}"));
        }
    }

    /// The workbook publishes its sheets and operations; the selection is
    /// not document state, so the edits bound to it are the surface's
    /// declarations, building the same action the on-screen control does.
    #[test]
    fn selection_edits_are_declarations_beside_the_published_workbook() {
        let doc = seeded();
        let published = &resources_for(&doc)[0];
        assert_eq!(published.kind, "sheets.workbook");
        assert!(published.actions.iter().any(|a| a.id == "new-sheet"));
        assert_eq!(published.children.len(), 2);
        assert!(published.children.iter().all(|c| c.kind == "sheets.sheet"));
        assert!(
            published.actions.iter().all(|a| a.id != "format-percent"),
            "selection edits are not the resource's own"
        );

        let mut app = crate::app::tests::attached_app();
        ui::widget::Widget::sync(&mut app, &doc, &automerge::ROOT);
        app.select_range_for_test((0, 0), (1, 1));
        let decls = app.selection_declarations();
        let percent = decls
            .iter()
            .find(|decl| decl.id == "format-percent")
            .expect("format the selection");
        let built = (percent.build)(&tilo_actions::ActionArgs::default(), 0).unwrap();
        assert_eq!(
            built,
            set_formats_action(
                0,
                stable(&doc, 0),
                [
                    ((0, 0), CellFormat::Percent),
                    ((1, 0), CellFormat::Percent),
                    ((0, 1), CellFormat::Percent),
                    ((1, 1), CellFormat::Percent),
                ]
            )
        );
    }

    /// Deleting a formula range's first or last row/column shrinks it;
    /// only deleting all of it yields `#REF!`.
    #[test]
    fn deleting_a_range_edge_row_or_column_shrinks_the_formula_range() {
        let mut doc = seeded();
        let sheet_id = stable(&doc, 0);
        // B2:B4 hold 1, 2, 4; D1 sums them and E1 sums row 3 across B:C.
        for (row, value) in [(1, "1"), (2, "2"), (3, "4")] {
            SheetsDoc::set_cell_raw(&mut doc, 0, (1, row), value);
        }
        SheetsDoc::set_cell_raw(&mut doc, 0, (2, 2), "8");
        SheetsDoc::set_cell_raw(&mut doc, 0, (3, 0), "=SUM(B2:B4)");
        SheetsDoc::set_cell_raw(&mut doc, 0, (4, 0), "=SUM($B3:C$3)");
        let value = |doc: &mut Doc, addr| {
            crate::formula::recalculate(&SheetsDoc::snapshot_sheet(doc, 0, APPLY_CURRENCY).raw)
                .get(&addr)
                .cloned()
        };

        // Deleting row 2, the range's first row, leaves B2:B3 = 2 + 4.
        dispatch(&mut doc, &delete_row_action(0, sheet_id.clone(), 1));
        assert_eq!(SheetsDoc::cell_raw(&doc, 0, (3, 0)), "=SUM(B2:B3)");
        assert_eq!(value(&mut doc, (3, 0)), Some(CellValue::Number(6.0)));
        // Deleting its last row leaves just B2.
        dispatch(&mut doc, &delete_row_action(0, sheet_id.clone(), 2));
        assert_eq!(SheetsDoc::cell_raw(&doc, 0, (3, 0)), "=SUM(B2:B2)");
        assert_eq!(value(&mut doc, (3, 0)), Some(CellValue::Number(2.0)));

        // E1 now reads `=SUM($B2:C$2)`; deleting column C, its last
        // column, keeps the formula (now in D1) alive on B2 alone.
        assert_eq!(SheetsDoc::cell_raw(&doc, 0, (4, 0)), "=SUM($B2:C$2)");
        dispatch(&mut doc, &delete_column_action(0, sheet_id.clone(), 2));
        assert_eq!(SheetsDoc::cell_raw(&doc, 0, (3, 0)), "=SUM($B2:B$2)");
        assert_eq!(value(&mut doc, (3, 0)), Some(CellValue::Number(2.0)));

        // Deleting every row the range covers is the only way to break it.
        dispatch(&mut doc, &delete_row_action(0, sheet_id, 1));
        assert_eq!(SheetsDoc::cell_raw(&doc, 0, (2, 0)), "=SUM(#REF!)");
    }

    /// Cell/chart/row actions write through, keep stable targets and remap formulas.
    #[test]
    fn cell_and_chart_actions_write_values_and_keep_targets() {
        // Case `set_cell_action_writes_the_value_and_format`.
        // A cell write through the action mutates the document.
        {
            let mut doc = seeded();
            let sheet_id = stable(&doc, 0);
            dispatch(
                &mut doc,
                &set_cell_action(0, sheet_id, (0, 1), "=SUM(B1:B3)", Some(CellFormat::Number)),
            );
            assert_eq!(SheetsDoc::cell_raw(&doc, 0, (0, 1)), "=SUM(B1:B3)");
            let snap = SheetsDoc::snapshot_sheet(&doc, 0, Currency::USD);
            assert_eq!(snap.fmt.get(&(0, 1)).copied(), Some(CellFormat::Number));
        }

        // Case `chart_editor_form_keeps_id_and_updates_title_kind_and_range`.
        {
            let action = edit_chart_form_action(
                4,
                "sheet-id",
                "00000000-0000-4000-8000-000000000099",
                "Revenue | line | D3:B1",
            )
            .expect("valid edit form");
            assert_eq!(
                action,
                SheetsAction::PutChart {
                    sheet: 4,
                    sheet_id: "sheet-id".into(),
                    chart_id: "00000000-0000-4000-8000-000000000099".into(),
                    title: "Revenue".into(),
                    kind: "line".into(),
                    c0: 1,
                    r0: 0,
                    c1: 3,
                    r1: 2
                }
            );
            assert!(edit_chart_form_action(0, "s", "id", "broken").is_none());
        }

        // Case `stable_cell_target_survives_a_concurrent_tab_reorder`.
        {
            let mut doc = seeded();
            let budget_id = stable(&doc, 0);
            let action = set_cell_action(0, budget_id, (7, 7), "on the intended sheet", None);

            // Another collaborator moved Budget after Sales before this envelope
            // replayed. The old positional index now names Sales, but the durable
            // id must still resolve Budget at its new index.
            SheetsDoc::move_sheet(&mut doc, 0, 1);
            dispatch(&mut doc, &action);

            assert_eq!(
                SheetsDoc::cell_raw(&doc, 1, (7, 7)),
                "on the intended sheet"
            );
            assert_eq!(SheetsDoc::cell_raw(&doc, 0, (7, 7)), "");
        }

        // Case `insert_and_delete_row_shift_cells`.
        // Inserting a row above the value cell shifts it down; delete shifts back.
        {
            let mut doc = seeded();
            let sheet_id = stable(&doc, 0);
            // Budget!A1 = "Rent". Insert a row above row 1 (at == 0): it moves to A2.
            dispatch(&mut doc, &insert_row_action(0, sheet_id.clone(), 0));
            assert_eq!(SheetsDoc::cell_raw(&doc, 0, (0, 0)), "");
            assert_eq!(SheetsDoc::cell_raw(&doc, 0, (0, 1)), "Rent");
            // Delete row 1 again: it shifts back up to A1.
            dispatch(&mut doc, &delete_row_action(0, sheet_id, 0));
            assert_eq!(SheetsDoc::cell_raw(&doc, 0, (0, 0)), "Rent");
        }

        // Case `row_and_column_actions_remap_formula_relationships`.
        {
            let mut doc = seeded();
            SheetsDoc::set_cell_raw(&mut doc, 0, (0, 1), "10");
            SheetsDoc::set_cell_raw(&mut doc, 0, (1, 2), "=SUM($A$1:A2)");
            let sheet_id = stable(&doc, 0);

            dispatch(&mut doc, &insert_row_action(0, sheet_id.clone(), 0));
            assert_eq!(
                SheetsDoc::cell_raw(&doc, 0, (1, 3)),
                "=SUM($A$2:A3)",
                "both anchored and relative formula references follow the inserted row"
            );

            dispatch(&mut doc, &delete_column_action(0, sheet_id, 0));
            assert_eq!(
                SheetsDoc::cell_raw(&doc, 0, (0, 3)),
                "=SUM(#REF!)",
                "a deleted referenced column is represented explicitly, never silently retargeted"
            );
        }

        // Case `cell_action_history_has_coordinate_resource`.
        {
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
    }

    /// New/duplicate/delete sheet identities converge, must be fresh, and the last sheet is kept.
    #[test]
    fn new_sheet_identities_converge_and_are_fresh() {
        // Case `authored_new_sheet_identity_converges_across_independent_replays`.
        {
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

        // Case `new_sheet_identity_must_be_a_fresh_uuid`.
        {
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
                let error = tilo_actions::apply_local(&mut doc, &action, encode(&action))
                    .expect_err("malformed or duplicate resource identity must be refused");
                assert!(matches!(error, ActionError::Refused(_)));
                assert_eq!(doc.save(), before);
            }
        }

        // Case `insert_and_duplicate_history_use_envelope_identities`.
        {
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
        }

        // Case `delete_sheet_action_removes_a_sheet_but_keeps_the_last`.
        // Deleting a sheet through its action removes it; the last sheet is kept.
        {
            let mut doc = seeded();
            assert_eq!(SheetsDoc::sheet_count(&doc), 2);
            let sheet_id = stable(&doc, 1);
            dispatch(&mut doc, &delete_sheet_action(1, sheet_id));
            assert_eq!(SheetsDoc::sheet_names(&doc), vec!["Budget"]);
            // The last sheet cannot be deleted away.
            let sheet_id = stable(&doc, 0);
            dispatch(&mut doc, &delete_sheet_action(0, sheet_id));
            assert_eq!(
                SheetsDoc::sheet_count(&doc),
                1,
                "a workbook keeps one sheet"
            );
        }
    }

    /// Hostile batches/imports are refused before writing; imports replay exactly; interchange runs as typed jobs.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn hostile_batches_and_imports_are_refused_atomically() {
        // Body of case `imported_workbook_has_an_exact_replay_cell_boundary` (a function so its early `return`/`?` stays local).
        fn imported_workbook_has_an_exact_replay_cell_boundary() {
            let doc = seeded();
            let rows = u32::try_from(MAX_IMPORTED_CELLS).expect("bounded import cap fits u32");
            let cells = (0..rows)
                .map(|row| CellWrite {
                    col: 0,
                    row,
                    raw: "1".into(),
                    fmt: None,
                })
                .collect();
            let mut action = SheetsAction::ImportSheets {
                replace_blank: false,
                source: None,
                sheets: vec![ImportedSheet {
                    name: "At the import limit".into(),
                    sheet_id: Uuid::new_v4().to_string(),
                    cells,
                }],
            };
            assert!(validate_action(&doc, &action).is_ok());
            let SheetsAction::ImportSheets { sheets, .. } = &mut action else {
                return;
            };
            sheets[0].cells.push(CellWrite {
                col: 1,
                row: 0,
                raw: "2".into(),
                fmt: None,
            });
            assert!(matches!(
                validate_action(&doc, &action),
                Err(ActionError::Refused(_))
            ));
        }

        // Case `hostile_batch_is_refused_without_a_partial_write`.
        {
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
            let error = tilo_actions::apply_local(&mut doc, &action, encode(&action))
                .expect_err("the full batch must be validated before any write");
            assert!(matches!(error, ActionError::Refused(_)));
            assert_eq!(
                doc.save(),
                before,
                "refusal leaves the CRDT byte-for-byte unchanged"
            );
        }

        // Case `hostile_import_refuses_before_creating_any_sheet`.
        {
            let mut doc = seeded();
            let before = doc.save();
            let action = SheetsAction::ImportSheets {
                replace_blank: false,
                source: None,
                sheets: vec![
                    ImportedSheet {
                        name: "Would exist".into(),
                        sheet_id: Uuid::new_v4().to_string(),
                        cells: vec![CellWrite {
                            col: 0,
                            row: 0,
                            raw: "safe".into(),
                            fmt: None,
                        }],
                    },
                    ImportedSheet {
                        name: "Bad later sheet".into(),
                        sheet_id: Uuid::new_v4().to_string(),
                        cells: vec![CellWrite {
                            col: MAX_COLUMN + 1,
                            row: 0,
                            raw: "outside grid".into(),
                            fmt: None,
                        }],
                    },
                ],
            };
            assert!(tilo_actions::apply_local(&mut doc, &action, encode(&action)).is_err());
            assert_eq!(
                doc.save(),
                before,
                "no prefix of an import can leak into the CRDT"
            );
        }

        // Case `imported_workbook_has_an_exact_replay_cell_boundary`.
        imported_workbook_has_an_exact_replay_cell_boundary();

        // Case `interchange_effects_are_typed_background_jobs`.
        {
            let specs = SheetsEffect::specs();
            for id in [
                "import-csv",
                "import-xlsx",
                "import-ods",
                "export-csv",
                "export-xlsx",
                "export-ods",
            ] {
                let action = specs.iter().find(|action| action.id == id).unwrap();
                assert_eq!(action.params()[0].name.as_ref(), "path");
                assert_eq!(
                    action.contract.execution,
                    ExecutionMode::Background { cancellable: true },
                    "{id} is a job its caller may cancel"
                );
                assert_eq!(action.contract.result, ResultSpec::Job);
                assert!(action.contract.validate_declaration().is_empty());
            }
        }
    }

    /// Resources exactly as the app publishes them for `doc`.
    fn resources_for(doc: &AutoCommit) -> Vec<Resource> {
        vec![published(
            &crate::resource::SheetsWorkbook::read(doc).expect("the workbook reads"),
        )]
    }

    /// `seeded` plus a second row of cells, so a restructure moves several.
    fn seeded_with_rows() -> AutoCommit {
        let mut doc = seeded();
        SheetsDoc::set_cell_raw(&mut doc, 0, (0, 2), "Food");
        SheetsDoc::set_cell_raw(&mut doc, 0, (1, 2), "300");
        doc.commit();
        doc
    }

    ui::action_contract_tests! {
        seed: seeded_with_rows(),
        registry: registry(),
        resources: |doc| resources_for(doc),
        palette: ["New sheet", "Rename sheet", "Delete sheet"],
        replay: [
            set_cell => |doc| encode(&set_cell_action(0, stable(doc, 0), (2, 4), "converge", None)),
            insert_row => |doc| encode(&insert_row_action(0, stable(doc, 0), 0)),
            set_style => |doc| encode(&SheetsAction::SetStyle {
                sheet_id: stable(doc, 0),
                c0: 0,
                r0: 0,
                c1: 1,
                r1: 2,
                bold: Some(true),
                fmt: Some("number".into()),
            }),
            delete_rows => |doc| encode(&SheetsAction::DeleteRows {
                sheet_id: stable(doc, 0),
                rows: vec![2, 0],
            }),
            batched_set_cells => |doc| encode(&set_cells_action(
                0,
                stable(doc, 0),
                vec![
                    ((0, 5), "=A1+1".to_string(), None),
                    ((1, 5), "42".to_string(), Some(CellFormat::Number)),
                    ((2, 5), "note".to_string(), Some(CellFormat::Text)),
                ],
            )),
            import_workbook => |_| encode(&import_sheets_action(vec![
                (
                    "Imported budget".into(),
                    vec![
                        CellWrite {
                            col: 0,
                            row: 0,
                            raw: "Rent".into(),
                            fmt: Some("text".into()),
                        },
                        CellWrite {
                            col: 1,
                            row: 0,
                            raw: "1200".into(),
                            fmt: Some("currency:USD".into()),
                        },
                    ],
                ),
                (
                    "Imported totals".into(),
                    vec![CellWrite {
                        col: 0,
                        row: 0,
                        raw: "=SUM(B1:B3)".into(),
                        fmt: None,
                    }],
                ),
            ])),
        ],
        undo: [
            add_sheet => |_| encode(&add_sheet_action("Fresh")),
            insert_column => |doc| encode(&insert_column_action(0, stable(doc, 0), 0)),
        ],
        encodes: [
            delete_sheet => ("sheets.sheet", "delete", |doc, _| {
                encode(&delete_sheet_action(0, stable(doc, 0)))
            }),
            rename_sheet => ("sheets.sheet", "rename", arg: "Q1", |doc, _| {
                encode(&rename_sheet_action(0, stable(doc, 0), "Q1"))
            }),
            insert_row => ("sheets.sheet", "insert-row", arg: "5", |doc, _| {
                encode(&insert_row_action(0, stable(doc, 0), 4))
            }),
        ],
    }
}
