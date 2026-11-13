//! Change notification for a directory Files shows: how much of a change a
//! watch reports, and waking the window when one arrives.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Coverage {
    None,
    Partial,
    Complete,
}

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
    target: Option<PathBuf>,
    watch: Option<tilo_files::Watch>,
    coverage: Coverage,
    waker: Waker,
}

impl DirectoryWatch {
    pub(crate) fn new(wake: ui::WakeHandle) -> Self {
        Self::with_waker(Waker::from(Arc::new(UiWake(wake))))
    }

    pub(crate) fn with_waker(waker: Waker) -> Self {
        Self {
            target: None,
            watch: None,
            coverage: Coverage::None,
            waker,
        }
    }

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

    pub(crate) fn adopt(&mut self, dir: &Path, started: Option<StartedWatch>) -> bool {
        self.target = Some(dir.to_path_buf());
        self.watch = None;
        self.coverage = Coverage::None;
        let Some(started) = started else {
            return false;
        };
        self.coverage = started.coverage;
        self.watch = Some(started.watch);
        // Arm the wake only once a drain has found a change, so a queued one wakes it.
        let _ = self.take_changes();
        true
    }

    pub(crate) fn target(&self) -> Option<&Path> {
        self.target.as_deref()
    }

    pub(crate) fn coverage(&self) -> Coverage {
        self.coverage
    }

    #[cfg(test)]
    pub(crate) fn assume_partial_coverage(&mut self) {
        if self.watch.is_some() {
            self.coverage = Coverage::Partial;
        }
    }

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

pub(crate) struct StartedWatch {
    watch: tilo_files::Watch,
    coverage: Coverage,
}

impl StartedWatch {
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

pub(crate) struct AwaitedDirectoryWatch {
    target: Option<PathBuf>,
    nearest: DirectoryWatch,
}

impl AwaitedDirectoryWatch {
    pub(crate) fn new(wake: ui::WakeHandle) -> Self {
        Self::with_waker(Waker::from(Arc::new(UiWake(wake))))
    }

    pub(crate) fn with_waker(waker: Waker) -> Self {
        Self {
            target: None,
            nearest: DirectoryWatch::with_waker(waker),
        }
    }

    pub(crate) fn point_at(&mut self, dir: Option<&Path>) -> bool {
        if self.target.as_deref() == dir {
            return false;
        }
        self.target = dir.map(Path::to_path_buf);
        self.settle()
    }

    pub(crate) fn coverage(&self) -> Coverage {
        self.nearest.coverage()
    }

    #[cfg(test)]
    pub(crate) fn watches_target(&self) -> bool {
        self.nearest.watch.is_some() && self.nearest.target == self.target
    }

    pub(crate) fn take_changes(&mut self) -> bool {
        let changed = self.nearest.take_changes();
        if changed {
            let _ = self.settle();
        }
        changed
    }

    fn settle(&mut self) -> bool {
        let Some(target) = self.target.clone() else {
            let _ = self.nearest.point_at(None);
            return false;
        };
        let mut started = false;
        for _ in target.ancestors() {
            let nearest = target
                .ancestors()
                .find(|dir| !dir.as_os_str().is_empty() && dir.is_dir());
            if self.nearest.point_at(nearest) {
                started = nearest == Some(target.as_path());
            } else if self.nearest.watch.is_some() || nearest.is_none_or(Path::is_dir) {
                break;
            }
        }
        started
    }
}

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

    pub(crate) fn take_changes(&self) -> bool {
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

        std::fs::create_dir_all(&target).unwrap();
        tilo_test_env::wait_until("the watch reaches the created directory", || {
            let _ = watch.take_changes();
            watch.watches_target()
        });
        std::fs::write(target.join("projection.json"), b"{}").unwrap();
        tilo_test_env::wait_until("a file in the directory is reported", || {
            watch.take_changes()
        });

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
