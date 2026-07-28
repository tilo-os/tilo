//! QR-image opening through the host file chooser.
//!
//! The portal client — worker thread, Tokio runtime, ashpd round-trip, and the
//! URI→path rule that makes this the file-access security boundary — lives in
//! [`ui::portal`]. All this module carries is the filter set and the labels
//! this app wants on the dialog.

pub use ui::portal::{PortalChannel, PortalOutcome, inject_for_test};

fn qr_image_filter() -> ui::portal::FileFilter {
    ui::portal::FileFilter::new("QR images")
        .mimetype("image/png")
        .mimetype("image/jpeg")
        .glob("*.png")
        .glob("*.jpg")
        .glob("*.jpeg")
}

/// Open the host chooser for a QR screenshot to decode, on a worker thread.
/// The chosen path arrives over the returned channel.
pub fn request_open() -> PortalChannel {
    ui::portal::PickRequest::open("Choose a QR image", "Open")
        .thread_name("totp-portal-open")
        .filter(qr_image_filter())
        .all_files()
        .send()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chooser_offers_qr_images_then_an_escape_hatch() {
        let r = ui::portal::PickRequest::open("Choose a QR image", "Open")
            .filter(qr_image_filter())
            .all_files();
        assert_eq!(r.title(), "Choose a QR image");
        assert!(!r.is_multiple());
        assert_eq!(r.filter_names(), vec!["QR images", "All files"]);
    }

    #[test]
    fn inject_drives_the_same_drain_path_the_worker_feeds() {
        let rx = inject_for_test(PortalOutcome::Cancelled);
        assert_eq!(rx.try_recv(), Ok(PortalOutcome::Cancelled));
    }
}
