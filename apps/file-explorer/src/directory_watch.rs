//! Change notification for a directory Files shows or reads.
//!
//! A watch on the directory's own entries (inotify on Linux, `FSEvents` on
//! macOS, through `tilo-files`) replaces re-reading it on a timer. The watch
//! thread wakes the UI through the window's [`ui::WakeHandle`], so an idle
//! window with an unchanged directory has nothing to do and no deadline.
//!
//! A watch only sees changes made through this machine's kernel. Where the
//! filesystem can change underneath it (a network filesystem, or a FUSE mount
//! that does not tell the kernel of its remote edits), the watch is
//! [`Coverage::Partial`]: local edits still show at once, and the caller keeps
//! polling for the rest. tilo-fs tells the kernel of each edit a peer made,
//! so a watch there is [`Coverage::Complete`]. The watch never opens a file,
//! so it hydrates nothing.

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
        Self::with_waker(Waker::from(Arc::new(UiWake(wake))))
    }

    /// A watch that wakes `waker` instead of a window, for a worker thread
    /// that keeps something current off the window's thread.
    pub(crate) fn with_waker(waker: Waker) -> Self {
        Self {
            target: None,
            watch: None,
            coverage: Coverage::None,
            waker,
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
        if let Some(dir) = dir {
            return self.adopt(dir, StartedWatch::start(dir));
        }
        self.target = None;
        self.watch = None;
        self.coverage = Coverage::None;
        false
    }

    /// Watch `dir` with `started`, the watch started for it (perhaps on
    /// another thread), or with nothing where it could not be watched.
    /// Returns `true` when a watch started, as [`Self::point_at`] does.
    pub(crate) fn adopt(&mut self, dir: &Path, started: Option<StartedWatch>) -> bool {
        self.target = Some(dir.to_path_buf());
        self.watch = None;
        self.coverage = Coverage::None;
        let Some(started) = started else {
            return false;
        };
        self.coverage = started.coverage;
        self.watch = Some(started.watch);
        // Arm the wake: the watch wakes the window only once a drain has
        // found its queue empty.
        let _ = self.take_changes();
        true
    }

    /// The directory the watch was last pointed at.
    pub(crate) fn target(&self) -> Option<&Path> {
        self.target.as_deref()
    }

    pub(crate) fn coverage(&self) -> Coverage {
        self.coverage
    }

    /// Treat the live watch as covering only local edits, as on a FUSE
    /// mount whose remote edits arrive underneath the kernel.
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

/// A watch on a directory's entries, started where waiting on the disk is
/// allowed, for a [`DirectoryWatch`] to adopt.
pub(crate) struct StartedWatch {
    watch: tilo_files::Watch,
    coverage: Coverage,
}

impl StartedWatch {
    /// Watch `dir`'s entries, or `None` where it cannot be watched.
    pub(crate) fn start(dir: &Path) -> Option<Self> {
        match tilo_files::native::watch_entries(dir) {
            Ok(watch) => Some(Self {
                watch,
                coverage: coverage_of(dir),
            }),
            Err(error) => {
                tracing::debug!(dir = %dir.display(), %error, "directory watch unavailable; polling");
                None
            }
        }
    }
}

/// A watch on a directory that may not exist yet, such as the trusted-identity
/// projection's, which the launcher binds into Files' sandbox only when there
/// is a projection to show.
///
/// While the directory is missing, its nearest existing ancestor is watched
/// instead, and every change there looks again for a closer directory, so
/// the directory's creation is seen as an event rather than found by polling
/// a path that is not there. A missing directory therefore costs an idle
/// window nothing, while one created later is still read at once.
pub(crate) struct AwaitedDirectoryWatch {
    target: Option<PathBuf>,
    nearest: DirectoryWatch,
}

impl AwaitedDirectoryWatch {
    pub(crate) fn new(wake: ui::WakeHandle) -> Self {
        Self::with_waker(Waker::from(Arc::new(UiWake(wake))))
    }

    /// An awaited watch that wakes `waker` instead of a window.
    pub(crate) fn with_waker(waker: Waker) -> Self {
        Self {
            target: None,
            nearest: DirectoryWatch::with_waker(waker),
        }
    }

    /// Await `dir`, or nothing for `None`. Pointing at the directory already
    /// awaited keeps its watch. Returns `true` when `dir` itself became
    /// watched: changes made before that are not reported, so the caller
    /// reads it once more.
    pub(crate) fn point_at(&mut self, dir: Option<&Path>) -> bool {
        if self.target.as_deref() == dir {
            return false;
        }
        self.target = dir.map(Path::to_path_buf);
        self.settle()
    }

    /// How much of the change the watch now held reports: that of the
    /// directory itself, or of the ancestor its creation will appear in.
    pub(crate) fn coverage(&self) -> Coverage {
        self.nearest.coverage()
    }

    /// Whether the awaited directory itself, not an ancestor, is watched.
    #[cfg(test)]
    pub(crate) fn watches_target(&self) -> bool {
        self.nearest.watch.is_some() && self.nearest.target == self.target
    }

    /// Drain every queued change and re-arm the wake. `true` when anything
    /// changed. A change also moves the watch to the closest existing
    /// directory on the way to the target, or back up to the nearest one
    /// left when the watched directory's own removal is reported (inotify
    /// reports it; `FSEvents` need not, and there the coverage is
    /// [`Coverage::Partial`], so the caller polls anyway).
    pub(crate) fn take_changes(&mut self) -> bool {
        let changed = self.nearest.take_changes();
        if changed {
            let _ = self.settle();
        }
        changed
    }

    /// Watch the target, or its nearest existing ancestor. `true` when the
    /// target itself became watched by this call.
    fn settle(&mut self) -> bool {
        let Some(target) = self.target.clone() else {
            let _ = self.nearest.point_at(None);
            return false;
        };
        let mut started = false;
        // Each pass either moves the watch, or finds the directory it looked
        // at gone and looks further up, so the depth of the path bounds the
        // passes. Another pass also catches a directory created between the
        // look and the watch.
        for _ in target.ancestors() {
            let nearest = target
                .ancestors()
                .find(|dir| !dir.as_os_str().is_empty() && dir.is_dir());
            if self.nearest.point_at(nearest) {
                started = nearest == Some(target.as_path());
            } else if self.nearest.watch.is_some() || nearest.is_none_or(Path::is_dir) {
                // Already watching it, or it cannot be watched for good.
                break;
            }
            // Otherwise it was removed before the watch was set up.
        }
        started
    }
}

/// Watches on a few directories, each standing for something read from it,
/// such as the record catalogue, whose generation a mount moves by changing
/// its root.
pub(crate) struct RootsWatch {
    watches: Vec<DirectoryWatch>,
    waker: Waker,
}

impl RootsWatch {
    pub(crate) fn new(wake: ui::WakeHandle) -> Self {
        Self {
            watches: Vec::new(),
            waker: Waker::from(Arc::new(UiWake(wake))),
        }
    }

    /// Watch each of `dirs`; pointing at the same directories keeps their
    /// watches. Returns `true` when a new watch started: changes made before
    /// it are not reported, so the caller reads once more.
    pub(crate) fn point_at(&mut self, dirs: &[PathBuf]) -> bool {
        let same = self.watches.len() == dirs.len()
            && self
                .watches
                .iter()
                .zip(dirs)
                .all(|(watch, dir)| watch.target() == Some(dir.as_path()));
        if same {
            return false;
        }
        self.watches = dirs
            .iter()
            .map(|dir| {
                let mut watch = DirectoryWatch::with_waker(self.waker.clone());
                let _ = watch.point_at(Some(dir));
                watch
            })
            .collect();
        self.watches.iter().any(|watch| watch.watch.is_some())
    }

    /// The least any of the watches reports; [`Coverage::None`] without any.
    pub(crate) fn coverage(&self) -> Coverage {
        self.watches
            .iter()
            .map(DirectoryWatch::coverage)
            .min_by_key(|coverage| match coverage {
                Coverage::None => 0,
                Coverage::Partial => 1,
                Coverage::Complete => 2,
            })
            .unwrap_or(Coverage::None)
    }

    /// Drain every watch and re-arm its wake. `true` when any reported a
    /// change.
    pub(crate) fn take_changes(&self) -> bool {
        // Every watch is drained, so each re-arms its wake.
        let mut changed = false;
        for watch in &self.watches {
            changed |= watch.take_changes();
        }
        changed
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

    #[test]
    fn an_entry_created_in_the_watched_directory_is_reported_once_drained() {
        let dir = tempfile::tempdir().unwrap();
        let mut watch = DirectoryWatch::new(ui::WakeHandle::detached());
        assert!(watch.point_at(Some(dir.path())));
        assert_ne!(watch.coverage(), Coverage::None);
        assert!(!watch.take_changes(), "nothing changed yet");
        // Making a directory is a single event, so once it is reported no
        // other event of this change can still be on its way.
        std::fs::create_dir(dir.path().join("new")).unwrap();
        tilo_test_env::wait_until("the watch to report the creation", || watch.take_changes());
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
    fn a_missing_directory_is_awaited_on_its_nearest_ancestor() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("a/b");
        let mut watch = AwaitedDirectoryWatch::new(ui::WakeHandle::detached());
        assert!(!watch.point_at(Some(&target)), "the target is missing");
        assert!(!watch.watches_target());
        assert_ne!(watch.coverage(), Coverage::None, "its ancestor is watched");
        assert!(
            !watch.point_at(Some(&target)),
            "the same target keeps its watch"
        );

        // Each change on the way moves the watch closer, one level created
        // at a time or several at once.
        std::fs::create_dir_all(&target).unwrap();
        tilo_test_env::wait_until("the watch reaches the created directory", || {
            let _ = watch.take_changes();
            watch.watches_target()
        });
        std::fs::write(target.join("projection.json"), b"{}").unwrap();
        tilo_test_env::wait_until("a file in the directory is reported", || {
            watch.take_changes()
        });

        // Removed again, the watch falls back to the ancestor still there.
        // Only inotify reports a watched directory's own removal.
        #[cfg(target_os = "linux")]
        {
            std::fs::remove_dir_all(root.path().join("a")).unwrap();
            tilo_test_env::wait_until("the watch falls back to an ancestor", || {
                let _ = watch.take_changes();
                !watch.watches_target() && watch.coverage() != Coverage::None
            });
        }
        assert!(!watch.point_at(None));
        assert_eq!(watch.coverage(), Coverage::None);
    }

    #[test]
    fn an_existing_directory_is_awaited_by_watching_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut watch = AwaitedDirectoryWatch::new(ui::WakeHandle::detached());
        assert!(
            watch.point_at(Some(dir.path())),
            "the target itself is watched"
        );
        assert!(watch.watches_target());
        assert!(!watch.take_changes(), "nothing changed yet");
    }

    #[test]
    fn a_change_in_any_watched_root_is_reported() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let roots = [first.path().to_path_buf(), second.path().to_path_buf()];
        let mut watch = RootsWatch::new(ui::WakeHandle::detached());
        assert_eq!(watch.coverage(), Coverage::None, "nothing watched yet");
        assert!(watch.point_at(&roots));
        assert!(!watch.point_at(&roots), "the same roots keep their watches");
        assert_ne!(watch.coverage(), Coverage::None);
        assert!(!watch.take_changes(), "nothing changed yet");
        std::fs::create_dir(second.path().join("new")).unwrap();
        tilo_test_env::wait_until("the second root's change is reported", || {
            watch.take_changes()
        });
        assert!(!watch.point_at(&[]));
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
