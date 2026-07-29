//! File picking for the two sides of a comparison, through the host chooser.
//!
//! The portal client — worker thread, Tokio runtime, ashpd round-trip, and the
//! URI→path rule that makes this the file-access security boundary — lives in
//! [`ui::portal`]. All this module carries is the filter set and the labels
//! this app wants on the dialog.

pub use ui::portal::{PortalChannel, PortalOutcome, inject_for_test};

fn text_filter() -> ui::portal::FileFilter {
    ui::portal::FileFilter::new("Text & source")
        .mimetype("text/plain")
        .glob("*.txt")
        .glob("*.md")
        .glob("*.rs")
        .glob("*.toml")
        .glob("*.json")
        .glob("*.yaml")
        .glob("*.yml")
        .glob("*.c")
        .glob("*.h")
        .glob("*.py")
        .glob("*.js")
        .glob("*.ts")
}

/// Open the host chooser for a file to compare, on a worker thread. The chosen
/// path arrives over the returned channel.
pub fn request_open() -> PortalChannel {
    ui::portal::PickRequest::open("Open a file to compare", "Open")
        .thread_name("diffmerge-portal-open")
        .filter(text_filter())
        .all_files()
        .send()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chooser_offers_text_then_an_escape_hatch() {
        // Any file can be diffed, so the escape hatch is not optional here.
        let r = ui::portal::PickRequest::open("Open a file to compare", "Open")
            .filter(text_filter())
            .all_files();
        assert_eq!(r.title(), "Open a file to compare");
        assert!(!r.is_multiple(), "one side at a time");
        assert_eq!(r.filter_names(), vec!["Text & source", "All files"]);
    }

    #[test]
    fn inject_drives_the_same_drain_path_the_worker_feeds() {
        let rx = inject_for_test(PortalOutcome::Cancelled);
        assert_eq!(rx.try_recv(), Ok(PortalOutcome::Cancelled));
    }
}
