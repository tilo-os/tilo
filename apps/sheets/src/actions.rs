//! What Sheets does that changes no workbook: reading and writing files, reading cells, moving
//! around the grid. Document changes are the workbook's field edits and verbs, and presentation
//! is never an action.

use ui::tilo_resource::{self, Effect};

/// What Sheets does that changes no workbook: file reads and writes, and moving around the grid.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "sheets.effect")]
pub enum SheetsEffect {
    /// Import CSV
    ///
    /// Adds a CSV file as a new sheet.
    #[effect(job, cancellable, opens_path = "csv")]
    ImportCsv {
        #[param("Absolute path of the file; omit to choose one")]
        path: Option<tilo_resource::AbsolutePath>,
    },
    /// Import Excel workbook
    ///
    /// Adds every sheet of an `.xlsx` workbook.
    #[effect(job, cancellable, opens_path = "xlsx")]
    ImportXlsx {
        #[param("Absolute path of the file; omit to choose one")]
        path: Option<tilo_resource::AbsolutePath>,
    },
    /// Import Notion export
    ///
    /// Adds every database of a Notion export as a sheet.
    #[effect(
        job,
        cancellable,
        opens_path = "zip",
        label = "sheets-notion-import-title"
    )]
    ImportNotion {
        #[param("Absolute path of the file; omit to choose one")]
        path: Option<tilo_resource::AbsolutePath>,
    },
    /// Adds every sheet of an `.ods` spreadsheet.
    #[effect(job, cancellable, opens_path = "ods")]
    ImportOds {
        #[param("Absolute path of the file; omit to choose one")]
        path: Option<tilo_resource::AbsolutePath>,
    },
    /// Export CSV
    ///
    /// Saves what the active sheet shows as a CSV file.
    #[effect(job, cancellable, risk = "export_risk")]
    ExportCsv {
        #[param("Absolute path to write it to; omit to choose")]
        path: Option<tilo_resource::AbsolutePath>,
    },
    /// Export Excel workbook
    ///
    /// Saves every sheet as an `.xlsx` workbook.
    #[effect(job, cancellable, risk = "export_risk")]
    ExportXlsx {
        #[param("Absolute path to write it to; omit to choose")]
        path: Option<tilo_resource::AbsolutePath>,
    },
    /// Saves every sheet as an `.ods` spreadsheet.
    #[effect(job, cancellable, risk = "export_risk")]
    ExportOds {
        #[param("Absolute path to write it to; omit to choose")]
        path: Option<tilo_resource::AbsolutePath>,
    },
    /// Import Excel workbook from content
    ///
    /// Adds every sheet of an `.xlsx` workbook the workspace holds as content, with its formulas,
    /// formats and hidden sheets; the same content is not added twice.
    #[effect(returns, kind = "undoable", audience = "agents")]
    ImportXlsxContent {
        #[param(content, "The workbook's content address, `tilo.content:<hash>`")]
        content: String,
        #[param("The workbook's title, e.g. the file's name, when it has none yet")]
        title: Option<String>,
    },
    /// Export Excel workbook as content
    ///
    /// Keeps every sheet as an `.xlsx` workbook in the content store, with no file to write.
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

/// What an export risks: nothing when the person picks the path themselves; writing over what a
/// caller names is destructive.
fn export_risk(effect: &SheetsEffect) -> ui::tilo_actions::RiskLevel {
    match effect {
        SheetsEffect::ExportCsv { path: Some(_) }
        | SheetsEffect::ExportXlsx { path: Some(_) }
        | SheetsEffect::ExportOds { path: Some(_) } => ui::tilo_actions::RiskLevel::Destructive,
        _ => ui::tilo_actions::RiskLevel::None,
    }
}
