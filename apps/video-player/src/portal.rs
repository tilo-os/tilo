//! Media opening through the host file chooser.
//!
//! The portal client — worker thread, Tokio runtime, ashpd round-trip, and the
//! URI→path rule that makes this the file-access security boundary — lives in
//! [`ui::portal`]. All this module carries is the filter set and the labels
//! this app wants on the dialog.
//!
//! A `/var/workspace/...` pick is an ordinary local path: the FUSE driver
//! hydrates it on read.

pub use ui::portal::{PortalChannel as OpenChannel, PortalOutcome as OpenOutcome};

#[cfg(test)]
pub use ui::portal::inject_for_test;

fn video_filter() -> ui::portal::FileFilter {
    ui::portal::FileFilter::new("Video")
        .mimetype("video/mp4")
        .mimetype("video/webm")
        .mimetype("video/x-matroska")
        .mimetype("video/quicktime")
        .glob("*.mp4")
        .glob("*.webm")
        .glob("*.mkv")
        .glob("*.mov")
        .glob("*.avi")
}

/// Open the host chooser for a video to play, on a worker thread. Without the
/// `portal` feature the shared client reports a failure rather than hanging,
/// so `--open PATH` remains the way in on a hermetic build.
pub fn request_open() -> OpenChannel {
    ui::portal::PickRequest::open("Open a video", "Play")
        .thread_name("video-player-portal")
        .filter(video_filter())
        .all_files()
        .send()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chooser_offers_video_then_an_escape_hatch() {
        let r = ui::portal::PickRequest::open("Open a video", "Play")
            .filter(video_filter())
            .all_files();
        assert_eq!(r.title(), "Open a video");
        assert!(!r.is_multiple());
        assert_eq!(r.filter_names(), vec!["Video", "All files"]);
    }

    #[test]
    fn inject_drives_the_same_drain_path_the_worker_feeds() {
        let rx = inject_for_test(OpenOutcome::Cancelled);
        assert_eq!(rx.try_recv(), Ok(OpenOutcome::Cancelled));
    }
}
