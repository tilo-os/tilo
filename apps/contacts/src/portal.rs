//! vCard import / export through the host file chooser.
//!
//! The portal client — worker thread, Tokio runtime, ashpd round-trip, and the
//! URI→path rule that makes this the file-access security boundary — lives in
//! [`ui::portal`]. All this module carries is the filter sets and the labels
//! this app wants on its two dialogs.
//!
//! Import is *multi-select*: the user picks arbitrarily many cards at once and
//! every local one comes back to be parsed and merged. Export is a Save dialog
//! with a suggested file name. The pure-Rust vCard parse/serialise in
//! [`crate::vcard`] is untouched by how files are chosen.

use ui::portal::{FileFilter, PickRequest};

pub use ui::portal::{
    MultiChannel as ImportChannel, MultiOutcome as ImportOutcome, PortalChannel as ExportChannel,
    PortalOutcome as ExportOutcome,
};

#[cfg(test)]
pub use ui::portal::{
    inject_for_test as inject_export_for_test, inject_multi_for_test as inject_import_for_test,
};

fn import_filter() -> FileFilter {
    FileFilter::new("vCard")
        .mimetype("text/vcard")
        .mimetype("text/x-vcard")
        .mimetype("text/directory")
        .glob("*.vcf")
        .glob("*.vcard")
}

fn export_filter() -> FileFilter {
    FileFilter::new("vCard")
        .mimetype("text/vcard")
        .glob("*.vcf")
        .glob("*.vcard")
}

/// Open the host multi-select chooser for contact files to import, on a worker
/// thread. Every local path the user picked arrives over the returned channel.
pub fn request_import() -> ImportChannel {
    PickRequest::open_multiple("Import contacts", "Import")
        .thread_name("contacts-portal-import")
        .filter(import_filter())
        .all_files()
        .send_multiple()
}

/// Open the host Save dialog for the address book, seeding the filename field
/// with `current_name`.
pub fn request_export(current_name: &str) -> ExportChannel {
    PickRequest::save("Export contacts", "Export", current_name)
        .thread_name("contacts-portal-export")
        .filter(export_filter())
        .send()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_is_multi_select_and_export_seeds_a_filename() {
        let import = PickRequest::open_multiple("Import contacts", "Import")
            .filter(import_filter())
            .all_files();
        assert_eq!(import.title(), "Import contacts");
        assert!(import.is_multiple(), "a user may import many cards at once");
        assert_eq!(import.filter_names(), vec!["vCard", "All files"]);

        // The Save dialog deliberately has no "All files" escape hatch: the
        // output is always a vCard.
        let export =
            PickRequest::save("Export contacts", "Export", "contacts.vcf").filter(export_filter());
        assert!(!export.is_multiple());
        assert_eq!(export.suggested_name(), Some("contacts.vcf"));
        assert_eq!(export.filter_names(), vec!["vCard"]);
    }

    #[test]
    fn inject_drives_the_same_drain_paths_the_workers_feed() {
        let rx = inject_import_for_test(ImportOutcome::Cancelled);
        assert_eq!(rx.try_recv(), Ok(ImportOutcome::Cancelled));
        let rx = inject_export_for_test(ExportOutcome::Cancelled);
        assert_eq!(rx.try_recv(), Ok(ExportOutcome::Cancelled));
    }
}
