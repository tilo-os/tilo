//! Following the files a pane shows, instead of reading them on a timer.
//!
//! The compositor and the relay keep what several panes show in small files
//! (the display and input snapshots, the relay's heartbeat). A pane that read
//! them every second woke the idle window every second to find nothing new.
//! [`Watched`] puts a change-only watch on their directories
//! ([`tilo_files::native::watch_changes`]): a change flags itself and wakes
//! the window, and nothing else does.
//!
//! A pane follows only while it is shown: the guard it holds
//! ([`crate::panes::model::PaneModel::follow`]) owns the watches, and the
//! last pane dropping it stops them.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What a change calls, on the watch's thread.
pub(crate) type OnChange = Arc<dyn Fn() + Send + Sync>;

/// Change-only watches over a set of files.
pub(crate) struct Watched {
    changed: Arc<AtomicBool>,
    complete: bool,
    _watches: Vec<tilo_files::native::ChangeWatch>,
}

impl Watched {
    /// Watch `files`: a change to any of them is flagged ([`Self::take`])
    /// and wakes the window. It starts flagged, so the first look reads
    /// them. Must be called on the UI thread, in the window's wake scope.
    pub(crate) fn files(files: &[PathBuf]) -> Self {
        let changed = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&changed);
        let wake = ui::WakeHandle::new();
        let on_change: OnChange = Arc::new(move || {
            flag.store(true, Ordering::Release);
            wake.wake();
        });
        Self::start(files, changed, &on_change)
    }

    /// Watch `files`, calling `on_change` on a watch's thread for every
    /// change; [`Self::take`] answers `false`.
    pub(crate) fn calling(files: &[PathBuf], on_change: &OnChange) -> Self {
        Self::start(files, Arc::new(AtomicBool::new(false)), on_change)
    }

    /// One watch per directory, every file of it named.
    fn start(files: &[PathBuf], changed: Arc<AtomicBool>, on_change: &OnChange) -> Self {
        let mut directories: Vec<(&Path, Vec<OsString>)> = Vec::new();
        // Files nobody could locate cannot be followed.
        let mut complete = !files.is_empty();
        for file in files {
            let (Some(directory), Some(name)) = (file.parent(), file.file_name()) else {
                complete = false;
                continue;
            };
            match directories
                .iter_mut()
                .find(|(known, _)| *known == directory)
            {
                Some((_, names)) => names.push(name.to_owned()),
                None => directories.push((directory, vec![name.to_owned()])),
            }
        }
        let mut watches = Vec::with_capacity(directories.len());
        for (directory, names) in directories {
            let notify = Arc::clone(on_change);
            match tilo_files::native::watch_changes(
                directory,
                move |path: &Path| {
                    path.file_name()
                        .is_some_and(|name| names.iter().any(|named| named == name))
                },
                move || notify(),
            ) {
                Ok(watch) => watches.push(watch),
                Err(error) => {
                    complete = false;
                    tracing::debug!(
                        target: "tilo::settings",
                        directory = %directory.display(),
                        %error,
                        "not watched; read on a timer instead"
                    );
                }
            }
        }
        Self {
            changed,
            complete,
            _watches: watches,
        }
    }

    /// Whether a watched file changed since the last call.
    pub(crate) fn take(&self) -> bool {
        self.changed.swap(false, Ordering::AcqRel)
    }

    /// Whether every file is watched. A pane whose files are not all
    /// watched reads them on its old timer, never not at all.
    pub(crate) fn is_complete(&self) -> bool {
        self.complete
    }

    /// Watches nothing and reports complete: a test flags it by hand
    /// ([`Self::touch`]).
    #[cfg(test)]
    pub(crate) fn by_hand() -> Self {
        Self {
            changed: Arc::new(AtomicBool::new(true)),
            complete: true,
            _watches: Vec::new(),
        }
    }

    /// Flag a change, as a watched file's would.
    #[cfg(test)]
    pub(crate) fn touch(&self) {
        self.changed.store(true, Ordering::Release);
    }
}

/// The guard in `slot` while a pane still holds it, else a new one from
/// `start`, kept in `slot` for the next pane. Every pane showing an area
/// shares one, so recomposing a pane never restarts what it follows.
pub(crate) fn shared<T: 'static>(slot: &mut Weak<T>, start: impl FnOnce() -> T) -> Rc<T> {
    if let Some(held) = slot.upgrade() {
        return held;
    }
    let started = Rc::new(start());
    *slot = Rc::downgrade(&started);
    started
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn counting() -> (OnChange, mpsc::Receiver<()>) {
        let (sender, changes) = mpsc::channel();
        let sender = std::sync::Mutex::new(sender);
        let on_change: OnChange = Arc::new(move || {
            if let Ok(sender) = sender.lock() {
                let _ = sender.send(());
            }
        });
        (on_change, changes)
    }

    fn settle(changes: &mpsc::Receiver<()>) {
        while changes.recv_timeout(Duration::from_millis(200)).is_ok() {}
    }

    #[test]
    fn a_named_file_changing_calls_and_an_unrelated_one_does_not() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("display-state.json");
        let other = root.path().join("sub").join("input-state.json");
        std::fs::create_dir(root.path().join("sub")).unwrap();
        let (on_change, changes) = counting();
        let watched = Watched::calling(&[first.clone(), other.clone()], &on_change);
        assert!(watched.is_complete(), "both directories are watched");
        assert!(!watched.take(), "a calling watch flags nothing");
        settle(&changes);

        std::fs::write(root.path().join("unrelated.json"), "{}").unwrap();
        assert!(
            changes.recv_timeout(Duration::from_millis(300)).is_err(),
            "a file nobody named changes nothing"
        );
        std::fs::write(&first, "{}").unwrap();
        changes.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        settle(&changes);
        std::fs::write(&other, "{}").unwrap();
        changes.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        settle(&changes);

        drop(watched);
        settle(&changes);
        std::fs::write(&first, "{\"again\":1}").unwrap();
        assert!(
            changes.recv_timeout(Duration::from_millis(300)).is_err(),
            "a dropped watch stops calling"
        );
    }

    #[test]
    fn a_flagging_watch_starts_flagged_and_flags_each_change_once() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("input-state.json");
        let watched = Watched::files(std::slice::from_ref(&file));
        assert!(watched.is_complete());
        assert!(watched.take(), "the first look reads the files");
        assert!(!watched.take(), "and only that one");
        std::fs::write(&file, "{}").unwrap();
        tilo_test_env::wait_until("the change to be flagged", || watched.take());
    }

    #[test]
    fn a_directory_that_cannot_be_watched_leaves_the_watch_incomplete() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("absent").join("display-state.json");
        let watched = Watched::files(&[missing]);
        assert!(!watched.is_complete(), "the pane falls back to its timer");
        assert!(
            !Watched::files(&[]).is_complete(),
            "nor are files nobody could locate"
        );
    }

    #[test]
    fn panes_share_one_guard_until_the_last_drops_it() {
        let mut slot = Weak::new();
        let mut started = 0;
        let first = shared(&mut slot, || {
            started += 1;
        });
        let second = shared(&mut slot, || {
            started += 1;
        });
        assert!(Rc::ptr_eq(&first, &second));
        drop((first, second));
        let _third = shared(&mut slot, || {
            started += 1;
        });
        assert_eq!(started, 2, "a guard nobody holds is started again");
    }
}
