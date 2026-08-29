//! File open / save through the host file chooser (XDG Desktop Portal).
//!
//! The portal client — worker thread, Tokio runtime, ashpd round-trip, and the
//! URI→path rule that makes this the file-access security boundary — lives in
//! [`ui::portal`]. This module only carries the labels and (deliberately
//! permissive) filter set this app wants on its two dialogs: a hex editor
//! opens and writes *arbitrary* binary files, so "All files" is the point.

use ui::portal::{FileFilter, PickRequest};

pub use ui::portal::{PortalChannel, PortalOutcome, inject_for_test};

/// Open the host chooser for any file to inspect, on a worker thread. The
/// chosen path arrives over the returned channel, drained once per frame.
pub fn request_open() -> PortalChannel {
    PickRequest::open("Open file", "Open")
        .thread_name("hex-portal-open")
        .filter(FileFilter::all_files())
        .send()
}

/// Open the host Save dialog, seeding the filename field with `suggested_name`.
/// The chosen destination comes back as [`PortalOutcome::Opened`].
pub fn request_save(suggested_name: String) -> PortalChannel {
    PickRequest::save("Save file", "Save", &suggested_name)
        .thread_name("hex-portal-save")
        .filter(FileFilter::all_files())
        .send()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::portal::PickRequest;

    #[test]
    fn open_dialog_accepts_any_file() {
        let open = PickRequest::open("Open file", "Open").filter(FileFilter::all_files());
        assert_eq!(open.title(), "Open file");
        assert!(!open.is_multiple());
        assert_eq!(open.filter_names(), vec!["All files"]);
        assert_eq!(
            open.suggested_name(),
            None,
            "an open dialog seeds no filename"
        );
    }

    #[test]
    fn save_dialog_seeds_the_current_name() {
        let save =
            PickRequest::save("Save file", "Save", "dump.bin").filter(FileFilter::all_files());
        assert_eq!(save.suggested_name(), Some("dump.bin"));
    }

    #[test]
    fn inject_drives_the_same_drain_path_the_worker_feeds() {
        let rx = inject_for_test(PortalOutcome::Cancelled);
        assert_eq!(rx.try_recv(), Ok(PortalOutcome::Cancelled));
    }
}
