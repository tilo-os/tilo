//! Image insertion through the host file chooser.
//!
//! The portal client — worker thread, Tokio runtime, ashpd round-trip, and the
//! URI→path rule that makes this the file-access security boundary — lives in
//! [`ui::portal`]. All this module carries is the filter set and the labels
//! this app wants on the dialog.

pub use ui::portal::{PortalChannel, PortalOutcome, inject_for_test};

/// The image formats the document can embed.
fn image_filter() -> ui::portal::FileFilter {
    ui::portal::FileFilter::new("Images")
        .glob("*.png")
        .glob("*.jpg")
        .glob("*.jpeg")
        .glob("*.gif")
        .glob("*.webp")
        .glob("*.bmp")
        .glob("*.svg")
}

/// Open the host image chooser on a worker thread and return the receiver the
/// widget drains each frame (see `WordProcessorWidget::drain_portal`). The UI
/// thread is never blocked.
pub fn request_open() -> PortalChannel {
    ui::portal::PickRequest::open("Insert image", "Insert")
        .thread_name("wp-portal-open")
        .filter(image_filter())
        .all_files()
        .send()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chooser_offers_images_then_an_escape_hatch() {
        let r = ui::portal::PickRequest::open("Insert image", "Insert")
            .filter(image_filter())
            .all_files();
        assert_eq!(r.title(), "Insert image");
        assert!(!r.is_multiple(), "one image at a time");
        assert_eq!(r.filter_names(), vec!["Images", "All files"]);
    }

    #[test]
    fn inject_drives_the_same_drain_path_the_worker_feeds() {
        let rx = inject_for_test(PortalOutcome::Cancelled);
        assert_eq!(rx.try_recv(), Ok(PortalOutcome::Cancelled));
    }
}
