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
    #[cfg(target_os = "linux")]
    {
        Ok(ChangeWatch {
            _watch: inotify_watch::DirectoryWatch::start(directory, names, on_change)?,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        use notify::{EventKind, RecursiveMode, Watcher as _};
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let Ok(event) = event else {
                    // An overflowed or failed queue may have hidden a change.
                    on_change();
                    return;
                };
                if matches!(event.kind, EventKind::Access(_)) {
                    return;
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
    use super::THREAD_NAME;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask, Watches};

    /// Writes, creation, removal and renames of the directory's entries.
    const MASK: WatchMask = WatchMask::CLOSE_WRITE
        .union(WatchMask::MODIFY)
        .union(WatchMask::CREATE)
        .union(WatchMask::DELETE)
        .union(WatchMask::MOVED_FROM)
        .union(WatchMask::MOVED_TO)
        .union(WatchMask::ONLYDIR);

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
            names: impl Fn(&Path) -> bool + Send + 'static,
            on_change: impl Fn() + Send + 'static,
        ) -> std::io::Result<Self> {
            let mut inotify = Inotify::init()?;
            let mut watches = inotify.watches();
            let descriptor = watches.add(directory, MASK)?;
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
