//! Playlist-cover picking through the host file chooser.
//!
//! The portal client — worker thread, Tokio runtime, ashpd round-trip, and the
//! URI→path rule that makes this the file-access security boundary — lives in
//! [`ui::portal`]. All this module carries is the filter set and the labels
//! this app wants on the dialog.

pub use ui::portal::{PortalChannel as CoverChannel, PortalOutcome as CoverOutcome};

#[cfg(test)]
pub use ui::portal::inject_for_test as inject_cover_for_test;

fn cover_image_filter() -> ui::portal::FileFilter {
    ui::portal::FileFilter::new("Images")
        .mimetype("image/png")
        .mimetype("image/jpeg")
        .mimetype("image/webp")
        .glob("*.png")
        .glob("*.jpg")
        .glob("*.jpeg")
        .glob("*.webp")
}

/// Open the host chooser for a playlist cover, on a worker thread. The caller
/// remembers which playlist the pick belongs to (see `MusicApp::cover_pick_for`)
/// so the outcome itself stays the same shape every app uses.
///
/// Without the `audio` feature the shared client reports a failure rather than
/// hanging, so a hermetic build still drains exactly one outcome.
pub fn request_cover() -> CoverChannel {
    ui::portal::PickRequest::open("Choose a playlist cover", "Set cover")
        .thread_name("music-portal-cover")
        .filter(cover_image_filter())
        .all_files()
        .send()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chooser_offers_images_then_an_escape_hatch() {
        let r = ui::portal::PickRequest::open("Choose a playlist cover", "Set cover")
            .filter(cover_image_filter())
            .all_files();
        assert_eq!(r.title(), "Choose a playlist cover");
        assert!(!r.is_multiple());
        assert_eq!(r.filter_names(), vec!["Images", "All files"]);
    }

    #[test]
    fn inject_drives_the_same_drain_path_the_worker_feeds() {
        let rx = inject_cover_for_test(CoverOutcome::Cancelled);
        assert_eq!(rx.try_recv(), Ok(CoverOutcome::Cancelled));
    }
}
