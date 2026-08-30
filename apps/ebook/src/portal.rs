//! Opening a book through the host file chooser.
//!
//! The portal client — worker thread, Tokio runtime, ashpd round-trip, and the
//! URI→path rule that makes this the file-access security boundary — lives in
//! [`ui::portal`]. All this module carries is the filter set and the label this
//! app wants on its "Open a book" dialog, so the reader can open an `.epub` from
//! *anywhere* — their device or the shared workspace — instead of a hidden
//! magic folder.

use ui::portal::{FileFilter, PickRequest};

pub use ui::portal::{PortalChannel, PortalOutcome, inject_for_test};

fn epub_filter() -> FileFilter {
    FileFilter::new("EPUB books")
        .mimetype("application/epub+zip")
        .glob("*.epub")
}

/// Open the host chooser for a book to read, on a worker thread. The chosen
/// path arrives over the returned channel, drained once per frame in the app's
/// `sync`. Never blocks the UI thread.
pub fn request_open() -> PortalChannel {
    PickRequest::open("Open a book", "Open")
        .thread_name("ebook-portal-open")
        .filter(epub_filter())
        .all_files()
        .send()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_open_dialog_is_configured_for_epubs() {
        let r = PickRequest::open("Open a book", "Open")
            .filter(epub_filter())
            .all_files();
        assert_eq!(r.title(), "Open a book");
        assert!(!r.is_multiple(), "opening a single book, not a batch");
        // The EPUB filter leads, with an "All files" escape hatch last so an
        // oddly-typed file is never unreachable.
        assert_eq!(r.filter_names(), ["EPUB books", "All files"]);
    }
}
