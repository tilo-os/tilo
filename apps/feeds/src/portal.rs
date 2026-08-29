//! OPML subscription import through the host file chooser.
//!
//! The portal client — worker thread, Tokio runtime, ashpd round-trip, and the
//! URI→path rule that makes this the file-access security boundary — lives in
//! [`ui::portal`]. All this module carries is the filter set and the labels
//! this app wants on the dialog.

pub use ui::portal::{
    PortalChannel as ImportChannel, PortalOutcome as ImportOutcome, inject_for_test,
};

fn opml_filter() -> ui::portal::FileFilter {
    ui::portal::FileFilter::new("OPML")
        .mimetype("text/x-opml")
        .mimetype("application/xml")
        .mimetype("text/xml")
        .glob("*.opml")
        .glob("*.xml")
}

/// Open the host chooser for an OPML file to import, on a worker thread. The
/// chosen path arrives over the returned channel.
pub fn request_import() -> ImportChannel {
    ui::portal::PickRequest::open("Import OPML", "Import")
        .thread_name("feeds-portal-import")
        .filter(opml_filter())
        .all_files()
        .send()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chooser_offers_opml_then_an_escape_hatch() {
        let r = ui::portal::PickRequest::open("Import OPML", "Import")
            .filter(opml_filter())
            .all_files();
        assert_eq!(r.title(), "Import OPML");
        assert!(!r.is_multiple());
        assert_eq!(r.filter_names(), vec!["OPML", "All files"]);
    }

    #[test]
    fn inject_drives_the_same_drain_path_the_worker_feeds() {
        let rx = inject_for_test(ImportOutcome::Cancelled);
        assert_eq!(rx.try_recv(), Ok(ImportOutcome::Cancelled));
    }
}
