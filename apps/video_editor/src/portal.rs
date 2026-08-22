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
pub use ui::portal::{PortalChannel as ExportDestChannel, PortalOutcome as ExportDestOutcome};

#[cfg(test)]
pub use ui::portal::inject_multi_for_test as inject_for_test;
#[cfg(test)]
pub use ui::portal::inject_for_test as inject_dest_for_test;

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

/// The chooser filter for one export container, so the Save dialog offers the
/// extension the encode will actually produce.
fn container_filter(ext: &str) -> FileFilter {
    let label = match ext {
        "mp4" => "MP4 video",
        "mov" => "QuickTime video",
        "webm" => "WebM video",
        other => return FileFilter::new(other).glob(&format!("*.{other}")),
    };
    let f = FileFilter::new(label).glob(&format!("*.{ext}"));
    match ext {
        "mp4" => f.mimetype("video/mp4"),
        "mov" => f.mimetype("video/quicktime"),
        "webm" => f.mimetype("video/webm"),
        _ => f,
    }
}

/// Ask the host where to write an export, on a worker thread.
///
/// Export previously had no destination UI at all: the output path came from
/// `QOS_VE_EXPORT_OUT` / `QOS_VE_EXPORT_DIR` or fell back to the temp
/// directory, so a user could not choose where their render landed and, under
/// `landlock = "strict"`, the app has no standing grant to write into
/// `~/Videos` anyway. Routing the destination through the Save portal is what
/// *earns* the write: the grant follows the file the user picked, which is the
/// same boundary [`request_import`] relies on for reads.
///
/// `suggested_name` seeds the filename field (the project name plus the
/// container's extension); `ext` also selects the type filter so the dialog
/// does not offer to save an H.264 file as `.webm`.
pub fn request_export_destination(suggested_name: &str, ext: &str) -> ExportDestChannel {
    PickRequest::save("Export video", "Export", suggested_name)
        .thread_name("video-editor-export-dest")
        .filter(container_filter(ext))
        .all_files()
        .send()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_destination_is_a_save_dialog_seeded_with_the_project_name() {
        let r = PickRequest::save("Export video", "Export", "Coastline_Doc_v3.mp4")
            .filter(container_filter("mp4"))
            .all_files();
        assert_eq!(r.title(), "Export video");
        assert!(!r.is_multiple(), "an export writes exactly one file");
        assert_eq!(
            r.suggested_name(),
            Some("Coastline_Doc_v3.mp4"),
            "the filename field is seeded so the user is not made to invent a name",
        );
        assert_eq!(r.filter_names(), vec!["MP4 video", "All files"]);
    }

    #[test]
    fn each_container_offers_its_own_extension() {
        // The dialog must not invite saving an H.264 stream as .webm — the
        // filter follows the container the encode chain will actually mux.
        assert_eq!(container_filter("mp4").name(), "MP4 video");
        assert_eq!(container_filter("mov").name(), "QuickTime video");
        assert_eq!(container_filter("webm").name(), "WebM video");
    }

    #[test]
    fn a_cancelled_export_dialog_drains_like_an_import_does() {
        let rx = inject_dest_for_test(ExportDestOutcome::Cancelled);
        assert_eq!(rx.try_recv(), Ok(ExportDestOutcome::Cancelled));
    }

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
