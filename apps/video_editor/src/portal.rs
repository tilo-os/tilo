//! Media import through the host file chooser.
//!
//! The portal client — the worker thread, the Tokio runtime, the `ashpd`
//! round-trip, and the URI→path rule that makes this the file-access security
//! boundary — lives in [`ui::portal`]. All this module carries is the filter
//! set and the labels this app wants on its multi-select import dialog.
//!
//! Import is *multi-select*: the user picks arbitrarily many clips at once and
//! every local one comes back to be probed and added to the media bin. A
//! `/var/workspace/...` pick resolves through the FUSE mount and hydrates on
//! read exactly like a local file (see [`crate::media`]).
//!
//! Reusing the shared primitive is not merely DRY: the previous per-app copy
//! mapped URIs with `strip_prefix("file://")`, which turns
//! `file://evil-host/etc/passwd` into a working-directory-relative path from a
//! URI that named a *remote host*. The shared [`ui::portal::to_local_path`]
//! rejects a foreign host outright, so converging on it closes that hole.

use ui::portal::{FileFilter, PickRequest};

pub use ui::portal::{MultiChannel as ImportChannel, MultiOutcome as ImportOutcome};

#[cfg(test)]
pub use ui::portal::inject_multi_for_test as inject_for_test;

/// The container / still formats the editor can bring into the bin. Both globs
/// and MIME types are supplied because portal backends differ in which they
/// honour.
fn media_filter() -> FileFilter {
    FileFilter::new("Media")
        .mimetype("video/mp4")
        .mimetype("video/webm")
        .mimetype("video/x-matroska")
        .mimetype("video/quicktime")
        .mimetype("audio/wav")
        .mimetype("audio/mpeg")
        .mimetype("image/png")
        .mimetype("image/jpeg")
        .glob("*.mp4")
        .glob("*.mov")
        .glob("*.webm")
        .glob("*.mkv")
        .glob("*.wav")
        .glob("*.mp3")
        .glob("*.png")
        .glob("*.jpg")
        .glob("*.jpeg")
}

/// Open the host multi-select chooser for media to import, on a worker thread.
/// Every local path the user picked arrives over the returned channel, which
/// the editor drains each frame (see `VideoEditorApp::drain_import`). The UI
/// thread is never blocked.
pub fn request_import() -> ImportChannel {
    PickRequest::open_multiple("Import media", "Import")
        .thread_name("video-editor-import")
        .filter(media_filter())
        .all_files()
        .send_multiple()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_is_multi_select_with_media_then_an_escape_hatch() {
        let r = PickRequest::open_multiple("Import media", "Import")
            .filter(media_filter())
            .all_files();
        assert_eq!(r.title(), "Import media");
        assert!(r.is_multiple(), "a user may import many clips at once");
        assert_eq!(r.filter_names(), vec!["Media", "All files"]);
    }

    #[test]
    fn inject_drives_the_same_drain_path_the_worker_feeds() {
        let rx = inject_for_test(ImportOutcome::Cancelled);
        assert_eq!(rx.try_recv(), Ok(ImportOutcome::Cancelled));
    }
}
