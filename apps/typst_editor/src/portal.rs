//! File open / save through the host file chooser (XDG Desktop Portal).
//!
//! The portal client — worker thread, Tokio runtime, ashpd round-trip, and the
//! URI→path rule that makes this the file-access security boundary — lives in
//! [`ui::portal`]. This module only carries the labels and filters the Typst
//! editor wants on its Open / Save / Export dialogs, plus the [`PendingIo`] tag
//! the app remembers so it knows what to do with the path the user picks.
//!
//! The editor autosaves its working document into the CRDT; these dialogs let
//! the user open an arbitrary `.typ` from disk, save the current source to a
//! chosen location, and export the compiled `.pdf` — all through the host
//! picker, per the qOS HIG, rather than the app's private library folder.

use ui::portal::{FileFilter, PickRequest};

pub use ui::portal::{PortalChannel, PortalOutcome, inject_for_test};

/// What the app should do with the path a pending portal round-trip returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingIo {
    /// Load the chosen `.typ` file into the editor.
    OpenTyp,
    /// Write the current source to the chosen `.typ` path.
    SaveTyp,
    /// Compile the current source and write the `.pdf` to the chosen path.
    ExportPdf,
}

fn typ_filter() -> FileFilter {
    FileFilter::new("Typst source").glob("*.typ")
}

fn pdf_filter() -> FileFilter {
    FileFilter::new("PDF document").glob("*.pdf")
}

/// Open the host chooser to pick a `.typ` file to open.
pub fn request_open() -> PortalChannel {
    PickRequest::open("Open a Typst file", "Open")
        .thread_name("typst-portal-open")
        .filter(typ_filter())
        .all_files()
        .send()
}

/// Open the host Save dialog for the source, seeded with `suggested`.
pub fn request_save(suggested: &str) -> PortalChannel {
    PickRequest::save("Save Typst file", "Save", suggested)
        .thread_name("typst-portal-save")
        .filter(typ_filter())
        .send()
}

/// Open the host Save dialog for the compiled PDF, seeded with `suggested`.
pub fn request_export_pdf(suggested: &str) -> PortalChannel {
    PickRequest::save("Export PDF", "Export", suggested)
        .thread_name("typst-portal-pdf")
        .filter(pdf_filter())
        .send()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_dialog_offers_typ_then_an_escape_hatch() {
        let r = PickRequest::open("Open a Typst file", "Open")
            .filter(typ_filter())
            .all_files();
        assert_eq!(r.filter_names(), vec!["Typst source", "All files"]);
        assert!(!r.is_multiple());
    }

    #[test]
    fn save_dialog_seeds_the_document_name() {
        let r = PickRequest::save("Save Typst file", "Save", "report.typ").filter(typ_filter());
        assert_eq!(r.suggested_name(), Some("report.typ"));
    }

    #[test]
    fn inject_drives_the_same_drain_path_the_worker_feeds() {
        let rx = inject_for_test(PortalOutcome::Cancelled);
        assert_eq!(rx.try_recv(), Ok(PortalOutcome::Cancelled));
    }
}
