//! Change notification for a directory Files shows or reads.
//!
//! A watch on the directory's own entries (inotify on Linux, `FSEvents` on
//! macOS, through `tilo-files`) replaces re-reading it on a timer. The watch
//! thread wakes the UI through the window's [`ui::WakeHandle`], so an idle
//! window with an unchanged directory has nothing to do and no deadline.
//!
//! A watch only sees changes made through this machine's kernel. Where the
//! filesystem can change underneath it (a FUSE mount such as tilo-fs, whose
//! remote edits arrive over gossip, or a network filesystem), the watch is
//! [`Coverage::Partial`]: local edits still show at once, and the caller keeps
//! polling for the rest. The watch never opens a file, so it hydrates nothing.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

/// How much of a directory's change a live watch reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Coverage {
    /// No watch: the directory must be polled.
    None,
    /// Local changes arrive as events; changes made elsewhere do not.
    Partial,
    /// Every change to the directory's entries arrives as an event.
    Complete,
}

/// Wakes the window that owns a watch. Notifications arrive on the watch's
/// own thread; this hands them to the UI's event loop.
struct UiWake(ui::WakeHandle);

impl Wake for UiWake {
    fn wake(self: Arc<Self>) {
        self.0.wake();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.wake();
    }
}

pub(crate) struct DirectoryWatch {
    /// The directory the watch was last pointed at, watched or not, so
    /// pointing at it again does not retry a failed watch on every sync.
    target: Option<PathBuf>,
    watch: Option<tilo_files::Watch>,
    coverage: Coverage,
    waker: Waker,
}

impl DirectoryWatch {
    pub(crate) fn new(wake: ui::WakeHandle) -> Self {
        Self {
            target: None,
            watch: None,
            coverage: Coverage::None,
            waker: Waker::from(Arc::new(UiWake(wake))),
        }
    }

    /// Watch `dir`'s entries, or nothing for `None`. Pointing at the
    /// directory already targeted keeps its watch. Returns `true` when a new
    /// watch started: changes made before it are not reported, so the caller
    /// reads the directory once more unless it is about to anyway.
    pub(crate) fn point_at(&mut self, dir: Option<&Path>) -> bool {
        if self.target.as_deref() == dir {
            return false;
        }
        self.target = dir.map(Path::to_path_buf);
        self.watch = None;
        self.coverage = Coverage::None;
        let Some(dir) = dir else {
            return false;
        };
        match tilo_files::native::watch_entries(dir) {
            Ok(watch) => {
                self.coverage = coverage_of(dir);
                self.watch = Some(watch);
                // Arm the wake: the watch wakes the window only once a
                // drain has found its queue empty.
                let _ = self.take_changes();
                true
            }
            Err(error) => {
                tracing::debug!(dir = %dir.display(), %error, "directory watch unavailable; polling");
                false
            }
        }
    }

    pub(crate) fn coverage(&self) -> Coverage {
        self.coverage
    }

    /// Treat the live watch as covering only local edits, as on a tilo-fs
    /// mount, whose remote edits arrive underneath the kernel.
    #[cfg(test)]
    pub(crate) fn assume_partial_coverage(&mut self) {
        if self.watch.is_some() {
            self.coverage = Coverage::Partial;
        }
    }

    /// Drain every queued change and re-arm the wake. `true` when anything
    /// changed, a rescan included.
    pub(crate) fn take_changes(&self) -> bool {
        let Some(watch) = &self.watch else {
            return false;
        };
        let mut cx = Context::from_waker(&self.waker);
        let mut changed = false;
        loop {
            let mut next = watch.next();
            match Pin::new(&mut next).poll(&mut cx) {
                Poll::Ready(_) => changed = true,
                Poll::Pending => return changed,
            }
        }
    }
}

fn coverage_of(dir: &Path) -> Coverage {
    if tilo_files::native::watch_sees_every_change(dir) {
        Coverage::Complete
    } else {
        Coverage::Partial
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_for_change(watch: &DirectoryWatch) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if watch.take_changes() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_file_created_in_the_watched_directory_is_reported_once_drained() {
        let dir = tempfile::tempdir().unwrap();
        let mut watch = DirectoryWatch::new(ui::WakeHandle::detached());
        assert!(watch.point_at(Some(dir.path())));
        assert_ne!(watch.coverage(), Coverage::None);
        assert!(!watch.take_changes(), "nothing changed yet");
        std::fs::write(dir.path().join("new.txt"), b"hello").unwrap();
        assert!(wait_for_change(&watch), "the creation is reported");
        std::thread::sleep(Duration::from_millis(100));
        let _ = watch.take_changes();
        assert!(!watch.take_changes(), "a drained watch reports nothing new");
    }

    #[test]
    fn pointing_at_the_same_directory_keeps_the_watch() {
        let dir = tempfile::tempdir().unwrap();
        let mut watch = DirectoryWatch::new(ui::WakeHandle::detached());
        assert!(watch.point_at(Some(dir.path())));
        assert!(!watch.point_at(Some(dir.path())));
        assert!(!watch.point_at(None));
        assert_eq!(watch.coverage(), Coverage::None);
    }

    #[test]
    fn a_missing_directory_has_no_watch_and_is_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let mut watch = DirectoryWatch::new(ui::WakeHandle::detached());
        assert!(!watch.point_at(Some(&missing)));
        assert_eq!(watch.coverage(), Coverage::None);
        assert!(!watch.take_changes());
    }
}
