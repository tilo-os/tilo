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
    #[effect(view, returns)]
    ReadCells {
        #[param("The sheet's id")]
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
