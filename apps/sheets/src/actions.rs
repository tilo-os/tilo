//! What Sheets does that changes no workbook: reading and writing
//! spreadsheet files, reading cells, and moving around the grid.
//!
//! Document changes are the workbook's own field edits and verbs
//! ([`crate::resource`]). Presentation — the selection (incl. multi-range),
//! scroll, which sheet tab is active, the formula-bar edit buffer until
//! Enter — is never an action.

use ui::tilo_resource::{self, Effect};

/// What Sheets does that changes no workbook: reading spreadsheet files into
/// new sheets and writing the workbook out as one, and moving around the grid
/// (views). An import reaches the workbook as one `import-sheets`.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "sheets.effect")]
pub enum SheetsEffect {
    /// Import CSV
    ///
    /// Adds a CSV file as a new sheet.
    #[effect(job, cancellable, opens_path = "csv")]
    ImportCsv {
        #[param("Path of the file; omit to choose one")]
        path: Option<String>,
    },
    /// Import Excel workbook
    ///
    /// Adds every sheet of an `.xlsx` workbook.
    #[effect(job, cancellable, opens_path = "xlsx")]
    ImportXlsx {
        #[param("Path of the file; omit to choose one")]
        path: Option<String>,
    },
    /// Adds every sheet of an `.ods` spreadsheet.
    #[effect(
        job,
        cancellable,
        opens_path = "ods",
        label = "Import OpenDocument spreadsheet"
    )]
    ImportOds {
        #[param("Path of the file; omit to choose one")]
        path: Option<String>,
    },
    /// Export CSV
    ///
    /// Saves what the active sheet shows as a CSV file.
    #[effect(job, cancellable, risk = "export_risk")]
    ExportCsv {
        #[param("Where to write it; omit to choose")]
        path: Option<String>,
    },
    /// Export Excel workbook
    ///
    /// Saves every sheet as an `.xlsx` workbook.
    #[effect(job, cancellable, risk = "export_risk")]
    ExportXlsx {
        #[param("Where to write it; omit to choose")]
        path: Option<String>,
    },
    /// Saves every sheet as an `.ods` spreadsheet.
    #[effect(
        job,
        cancellable,
        risk = "export_risk",
        label = "Export OpenDocument spreadsheet"
    )]
    ExportOds {
        #[param("Where to write it; omit to choose")]
        path: Option<String>,
    },
    /// Import Excel workbook from content
    ///
    /// Adds every sheet of an `.xlsx` workbook the room holds as content (an
    /// attachment, a shared file, an export), with its formulas, money,
    /// percent and date formats and hidden sheets; a new workbook's untouched
    /// blank sheet gives way to them. The same content imported again is
    /// not added twice.
    #[effect(returns, kind = "undoable", audience = "agents")]
    ImportXlsxContent {
        #[param(content, "The workbook's content address, `tilo.content:<hash>`")]
        content: String,
        #[param("The workbook's title, e.g. the file's name, when it has none yet")]
        title: Option<String>,
    },
    /// Export Excel workbook as content
    ///
    /// Keeps every sheet as an `.xlsx` workbook in the content store, with no
    /// file to write: the caller reads, attaches or sends it by the content
    /// address it answers.
    #[effect(content, kind = "read", idempotent)]
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
    /// Read cells
    ///
    /// What a range of a sheet holds and shows: computed values row by row,
    /// the formulas and the bold cells by reference.
    #[effect(view, returns, audience = "agents")]
    ReadCells {
        #[param("The sheet's id or name")]
        sheet: String,
        #[param("Cell or range, e.g. A1:D6; every used cell when none")]
        range: Option<String>,
    },
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

/// What an export risks: nothing when the person picks where in the save
/// dialog, which is itself the confirmation; writing over whatever is at a
/// path a caller names is destructive and asks first.
fn export_risk(effect: &SheetsEffect) -> ui::tilo_actions::RiskLevel {
    match effect {
        SheetsEffect::ExportCsv { path: Some(_) }
        | SheetsEffect::ExportXlsx { path: Some(_) }
        | SheetsEffect::ExportOds { path: Some(_) } => ui::tilo_actions::RiskLevel::Destructive,
        _ => ui::tilo_actions::RiskLevel::None,
    }
}
