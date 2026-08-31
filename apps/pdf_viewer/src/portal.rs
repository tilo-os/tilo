//! PDF open / page import / annotated save through the host file chooser.
//!
//! The portal client — worker thread, Tokio runtime, ashpd round-trip, and the
//! URI→path rule that makes this the file-access security boundary — lives in
//! [`ui::portal`]. All this module carries is the filter sets and the labels
//! this app wants on each of its three dialogs.

use ui::portal::{FileFilter, PickRequest};

pub use ui::portal::{PortalChannel, PortalOutcome, inject_for_test};

fn pdf_filter() -> FileFilter {
    FileFilter::new("PDF documents").mimetype("application/pdf")
}

fn pdf_or_image_filter() -> FileFilter {
    FileFilter::new("PDF or image")
        .mimetype("application/pdf")
        .mimetype("image/png")
        .mimetype("image/jpeg")
}

/// Open the host chooser for a PDF to view, on a worker thread. The chosen
/// path arrives over the returned channel.
pub fn request_open() -> PortalChannel {
    PickRequest::open("Open a PDF", "Open")
        .thread_name("pdf-portal-open")
        .filter(pdf_filter())
        .all_files()
        .send()
}

/// Open the host chooser for pages to append — a PDF or an image. Same
/// non-blocking contract as [`request_open`].
pub fn request_import() -> PortalChannel {
    PickRequest::open("Import pages from a PDF or image", "Import")
        .thread_name("pdf-portal-import")
        .filter(pdf_or_image_filter())
        .all_files()
        .send()
}

/// Open the host Save dialog for the annotated document, seeding the filename
/// field with `suggested_name`. The chosen destination comes back as
/// [`PortalOutcome::Opened`].
pub fn request_save(suggested_name: &str) -> PortalChannel {
    PickRequest::save("Save annotated PDF", "Save", suggested_name)
        .thread_name("pdf-portal-save")
        .filter(pdf_filter())
        .send()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_dialog_is_configured_for_its_own_job() {
        let open = PickRequest::open("Open a PDF", "Open")
            .filter(pdf_filter())
            .all_files();
        assert_eq!(open.title(), "Open a PDF");
        assert!(!open.is_multiple());
        assert_eq!(open.filter_names(), vec!["PDF documents", "All files"]);
        assert_eq!(
            open.suggested_name(),
            None,
            "an open dialog seeds no filename"
        );

        let import = PickRequest::open("Import pages from a PDF or image", "Import")
            .filter(pdf_or_image_filter())
            .all_files();
        assert_eq!(import.filter_names(), vec!["PDF or image", "All files"]);

        // The Save dialog deliberately has no "All files" escape hatch: the
        // output is always a PDF.
        let save =
            PickRequest::save("Save annotated PDF", "Save", "notes.pdf").filter(pdf_filter());
        assert_eq!(save.suggested_name(), Some("notes.pdf"));
        assert_eq!(save.filter_names(), vec!["PDF documents"]);
    }

    #[test]
    fn inject_drives_the_same_drain_path_the_worker_feeds() {
        let rx = inject_for_test(PortalOutcome::Cancelled);
        assert_eq!(rx.try_recv(), Ok(PortalOutcome::Cancelled));
    }
}
