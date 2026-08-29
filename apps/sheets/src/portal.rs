//! File open / save through the host file chooser (XDG Desktop Portal).
//!
//! The portal client — worker thread, Tokio runtime, ashpd round-trip, and the
//! URI→path rule that makes this the file-access security boundary — lives in
//! [`ui::portal`]. This module only carries the labels and per-format filters
//! Sheets wants on its import/save dialogs, plus the [`PendingIo`] tag the app
//! remembers so it knows what to do with the path the user picks.
//!
//! Sheets keeps its own workbook in an autosaved CRDT; these dialogs are for
//! **interchange** — importing a `.csv` / `.xlsx` / `.ods` a user has on disk,
//! or exporting the active sheet to one. Previously both keyed off a fixed
//! `~/Documents` folder with no picker; routing through the portal lets the
//! user choose a real location and makes the per-workspace sandbox prompt the
//! access boundary, per the qOS HIG.

use ui::portal::{FileFilter, PickRequest};

pub use ui::portal::{PortalChannel, PortalOutcome, inject_for_test};

/// The interchange formats Sheets can import from / export to on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interchange {
    /// Comma-separated values (the active sheet only).
    Csv,
    /// Microsoft Excel workbook.
    Xlsx,
    /// `LibreOffice` Calc spreadsheet.
    Ods,
}

impl Interchange {
    /// The file extension for this format.
    pub fn ext(self) -> &'static str {
        match self {
            Interchange::Csv => "csv",
            Interchange::Xlsx => "xlsx",
            Interchange::Ods => "ods",
        }
    }

    /// The chooser's type filter for this format.
    fn filter(self) -> FileFilter {
        match self {
            Interchange::Csv => FileFilter::new("CSV").glob("*.csv"),
            Interchange::Xlsx => FileFilter::new("Excel workbook").glob("*.xlsx"),
            Interchange::Ods => FileFilter::new("Calc spreadsheet").glob("*.ods"),
        }
    }
}

/// What the app should do with the path a pending portal round-trip returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingIo {
    /// Import a spreadsheet in this format into a new sheet.
    Import(Interchange),
    /// Export the active sheet to this format.
    Export(Interchange),
}

/// Open the host chooser to pick a file to import, on a worker thread. The
/// chosen path arrives over the returned channel, drained once per frame.
pub fn request_import(fmt: Interchange) -> PortalChannel {
    PickRequest::open("Import spreadsheet", "Import")
        .thread_name("sheets-portal-import")
        .filter(fmt.filter())
        .all_files()
        .send()
}

/// Open the host Save dialog, seeding the filename field with `suggested`. The
/// chosen destination comes back as [`PortalOutcome::Opened`].
pub fn request_export(fmt: Interchange, suggested: &str) -> PortalChannel {
    PickRequest::save("Export spreadsheet", "Export", suggested)
        .thread_name("sheets-portal-export")
        .filter(fmt.filter())
        .send()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_dialog_offers_the_format_then_an_escape_hatch() {
        let r = PickRequest::open("Import spreadsheet", "Import")
            .filter(Interchange::Xlsx.filter())
            .all_files();
        assert_eq!(r.title(), "Import spreadsheet");
        assert!(!r.is_multiple(), "one workbook at a time");
        assert_eq!(r.filter_names(), vec!["Excel workbook", "All files"]);
    }

    #[test]
    fn export_dialog_seeds_the_sheet_name() {
        let r = PickRequest::save("Export spreadsheet", "Export", "Budget.csv")
            .filter(Interchange::Csv.filter());
        assert_eq!(r.suggested_name(), Some("Budget.csv"));
    }

    #[test]
    fn inject_drives_the_same_drain_path_the_worker_feeds() {
        let rx = inject_for_test(PortalOutcome::Cancelled);
        assert_eq!(rx.try_recv(), Ok(PortalOutcome::Cancelled));
    }
}
