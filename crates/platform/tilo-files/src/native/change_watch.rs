//! A change-only watch on one native directory, for a long-lived reader that
//! re-reads a few files when they change: a callback on a thread of its own,
//! where [`crate::Watch`] is an async stream.
//!
//! On Linux the watch asks inotify for changes only: writes, creation,
//! removal and renames. Opening and reading a file are left out, and so are
//! attribute changes. The `notify` crate's inotify watch always includes
//! opens, so its thread woke for every read of any file in the watched
//! directory, the watching process's own included. Elsewhere `notify` reports
//! the platform's events, and its access events are dropped.
//!
//! A reader that only cares which entries exist (a socket that is to appear,
//! a device node, the directories on the way to a file) asks for
//! [`watch_presence`], which leaves writes out too. Writes to an entry of
//! `/dev` (every reply of a FUSE server to `/dev/fuse`, every write to
//! `/dev/null` or `/dev/ptmx`) or of a home folder are frequent, and each one
//! would otherwise wake the watch's thread for nothing.

use std::path::Path;

/// The name of a watch's thread (Linux), as it shows in a wakeup report.
pub const THREAD_NAME: &str = "tilo-dir-watch";

/// A live change watch; dropping it stops notifications.
pub struct ChangeWatch {
    #[cfg(target_os = "linux")]
    _watch: inotify_watch::DirectoryWatch,
    #[cfg(not(target_os = "linux"))]
    _watcher: notify::RecommendedWatcher,
}

/// What a watch reports about the entries of its directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reported {
    /// Writes, creation, removal and renames.
    Changes,
    /// Creation, removal and renames only.
    Entries,
}

/// Call `on_change` when an entry of `directory` that `names` accepts (given
/// its full path) is written, created, removed or renamed, and when changes
/// may have been missed (an overflowed or failed queue).
///
/// # Errors
///
/// The platform's failure to watch `directory`.
pub fn watch_changes(
    directory: &Path,
    names: impl Fn(&Path) -> bool + Send + 'static,
    on_change: impl Fn() + Send + 'static,
) -> std::io::Result<ChangeWatch> {
    watch(directory, Reported::Changes, names, on_change)
}

/// Call `on_change` when an entry of `directory` that `names` accepts (given
/// its full path) is created, removed or renamed, and when changes may have
/// been missed. Writes to the entries are not reported.
///
/// # Errors
///
/// The platform's failure to watch `directory`.
pub fn watch_presence(
    directory: &Path,
    names: impl Fn(&Path) -> bool + Send + 'static,
    on_change: impl Fn() + Send + 'static,
) -> std::io::Result<ChangeWatch> {
    watch(directory, Reported::Entries, names, on_change)
}

fn watch(
    directory: &Path,
    reported: Reported,
    names: impl Fn(&Path) -> bool + Send + 'static,
    on_change: impl Fn() + Send + 'static,
) -> std::io::Result<ChangeWatch> {
    #[cfg(target_os = "linux")]
    {
        Ok(ChangeWatch {
            _watch: inotify_watch::DirectoryWatch::start(directory, reported, names, on_change)?,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        use notify::event::ModifyKind;
        use notify::{EventKind, RecursiveMode, Watcher as _};
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let Ok(event) = event else {
                    // An overflowed or failed queue may have hidden a change.
                    on_change();
                    return;
                };
                match event.kind {
                    EventKind::Access(_) => return,
                    EventKind::Modify(ModifyKind::Name(_)) => {}
                    EventKind::Modify(_) if reported == Reported::Entries => return,
                    _ => {}
                }
                if event.need_rescan() || event.paths.iter().any(|path| names(path)) {
                    on_change();
                }
            })
            .map_err(std::io::Error::other)?;
        watcher
            .watch(directory, RecursiveMode::NonRecursive)
            .map_err(std::io::Error::other)?;
        Ok(ChangeWatch { _watcher: watcher })
    }
}

#[cfg(target_os = "linux")]
mod inotify_watch {
    use super::{Reported, THREAD_NAME};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask, Watches};

    /// Creation, removal and renames of the directory's entries.
    const ENTRIES: WatchMask = WatchMask::CREATE
        .union(WatchMask::DELETE)
        .union(WatchMask::MOVED_FROM)
        .union(WatchMask::MOVED_TO)
        .union(WatchMask::ONLYDIR);

    /// Writes to the directory's entries besides [`ENTRIES`].
    const CHANGES: WatchMask = ENTRIES
        .union(WatchMask::CLOSE_WRITE)
        .union(WatchMask::MODIFY);

    /// One directory watched by a thread of its own, which blocks in `read` until the kernel
    /// reports a change.
    pub(super) struct DirectoryWatch {
        watches: Watches,
        descriptor: WatchDescriptor,
        stopped: Arc<AtomicBool>,
    }

    impl DirectoryWatch {
        pub(super) fn start(
            directory: &Path,
            reported: Reported,
            names: impl Fn(&Path) -> bool + Send + 'static,
            on_change: impl Fn() + Send + 'static,
        ) -> std::io::Result<Self> {
            let mut inotify = Inotify::init()?;
            let mut watches = inotify.watches();
            let mask = match reported {
                Reported::Changes => CHANGES,
                Reported::Entries => ENTRIES,
            };
            let descriptor = watches.add(directory, mask)?;
            let stopped = Arc::new(AtomicBool::new(false));
            let stop = Arc::clone(&stopped);
            let root = directory.to_owned();
            std::thread::Builder::new()
                .name(THREAD_NAME.to_owned())
                .spawn(move || {
                    let mut buffer = [0_u8; 4096];
                    while !stop.load(Ordering::Acquire) {
                        let Ok(events) = inotify.read_events_blocking(&mut buffer) else {
                            // The descriptor failed: nothing more will be reported.
                            on_change();
                            return;
                        };
                        let mut changed = false;
                        for event in events {
                            if event.mask.contains(EventMask::IGNORED) {
                                // Dropped, or the directory itself went away.
                                return;
                            }
                            // An overflowed queue may have hidden a change; re-reading is
                            // cheap.
                            changed |= event.mask.contains(EventMask::Q_OVERFLOW)
                                || event.name.is_some_and(|name| names(&root.join(name)));
                        }
                        if changed && !stop.load(Ordering::Acquire) {
                            on_change();
                        }
                    }
                })?;
            Ok(Self {
                watches,
                descriptor,
                stopped,
            })
        }
    }

    impl Drop for DirectoryWatch {
        fn drop(&mut self) {
            self.stopped.store(true, Ordering::Release);
            // Removing the watch queues `IN_IGNORED`, which wakes the thread to see `stopped`.
            let _ = self.watches.remove(self.descriptor.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// How long a loaded test machine may take to report a change.
    const REPORT: Duration = Duration::from_secs(10);
    /// How long a change that should not be reported is given to arrive anyway.
    const QUIET: Duration = Duration::from_millis(500);

    fn counted() -> (Arc<AtomicUsize>, impl Fn() + Send + 'static) {
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        (count, move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })
    }

    fn reaches(count: &AtomicUsize, at_least: usize) -> bool {
        let started = Instant::now();
        while started.elapsed() < REPORT {
            if count.load(Ordering::SeqCst) >= at_least {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_changes_watch_reports_a_write_to_an_entry() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let file = dir.path().join("value");
        std::fs::write(&file, b"one").expect("write");
        let (count, on_change) = counted();
        let _watch = watch_changes(dir.path(), |_| true, on_change).expect("watch");
        std::fs::write(&file, b"two").expect("rewrite");
        assert!(reaches(&count, 1), "a write to an entry is a change");
    }

    #[test]
    fn an_entries_watch_reports_creation_and_removal_but_not_writes() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let file = dir.path().join("busy");
        std::fs::write(&file, b"one").expect("write");
        let (count, on_change) = counted();
        let _watch = watch_presence(dir.path(), |_| true, on_change).expect("watch");
        for round in 0..20 {
            std::fs::write(&file, format!("{round}")).expect("rewrite");
        }
        std::thread::sleep(QUIET);
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "writes to an existing entry wake nothing"
        );
        std::fs::write(dir.path().join("new"), b"").expect("create");
        assert!(reaches(&count, 1), "a new entry is reported");
        let seen = count.load(Ordering::SeqCst);
        std::fs::remove_file(&file).expect("remove");
        assert!(reaches(&count, seen + 1), "a removed entry is reported");
    }
}
