//! A change-only watch on a native directory, or on a whole tree, for a
//! long-lived reader that re-reads a few files when they change: a callback
//! on a thread of its own, where [`crate::Watch`] is an async stream.
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
//!
//! A reader of files nested in subdirectories (a package's manifest in its
//! own folder) asks for [`watch_tree_changes`]: every directory below the
//! root is watched as well, a subdirectory created or moved in from then on
//! as soon as it is reported, without following links.

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

/// How deep a watch reaches below its directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Depth {
    /// The directory's own entries.
    Entries,
    /// The directory's entries and those of every directory below it.
    Tree,
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
/// its full path) is written, created, removed or renamed, when `directory`
/// itself is removed or moved away (the watch ends then), and when changes
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
    watch(
        directory,
        Depth::Entries,
        Reported::Changes,
        names,
        on_change,
    )
}

/// [`watch_changes`] for every entry below `root`, at any depth: an entry of
/// a subdirectory, one created later included. `names` is given each changed
/// entry's full path.
///
/// # Errors
///
/// The platform's failure to watch `root` or a directory below it.
pub fn watch_tree_changes(
    root: &Path,
    names: impl Fn(&Path) -> bool + Send + 'static,
    on_change: impl Fn() + Send + 'static,
) -> std::io::Result<ChangeWatch> {
    watch(root, Depth::Tree, Reported::Changes, names, on_change)
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
    watch(
        directory,
        Depth::Entries,
        Reported::Entries,
        names,
        on_change,
    )
}

fn watch(
    directory: &Path,
    depth: Depth,
    reported: Reported,
    names: impl Fn(&Path) -> bool + Send + 'static,
    on_change: impl Fn() + Send + 'static,
) -> std::io::Result<ChangeWatch> {
    #[cfg(target_os = "linux")]
    {
        Ok(ChangeWatch {
            _watch: inotify_watch::DirectoryWatch::start(
                directory, depth, reported, names, on_change,
            )?,
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
        let mode = match depth {
            Depth::Entries => RecursiveMode::NonRecursive,
            Depth::Tree => RecursiveMode::Recursive,
        };
        watcher
            .watch(directory, mode)
            .map_err(std::io::Error::other)?;
        Ok(ChangeWatch { _watcher: watcher })
    }
}

#[cfg(target_os = "linux")]
mod inotify_watch {
    use super::{Depth, Reported, THREAD_NAME};
    use std::collections::HashMap;
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask, Watches};

    /// Creation, removal and renames of the directory's entries.
    const ENTRIES: WatchMask = WatchMask::CREATE
        .union(WatchMask::DELETE)
        .union(WatchMask::MOVED_FROM)
        .union(WatchMask::MOVED_TO)
        .union(WatchMask::ONLYDIR);

    /// Writes to the directory's entries besides [`ENTRIES`]. Not
    /// `IN_CLOSE_WRITE`: every write already reports `IN_MODIFY`, and a file
    /// opened for writing and closed unchanged (a lock file, a store opened
    /// read-write to be read) changed nothing.
    const CHANGES: WatchMask = ENTRIES.union(WatchMask::MODIFY);

    /// One directory, or a tree, watched by a thread of its own, which blocks in `read` until
    /// the kernel reports a change.
    pub(super) struct DirectoryWatch {
        watches: Watches,
        descriptor: WatchDescriptor,
        stopped: Arc<AtomicBool>,
    }

    /// The directories of a tree watch, by their watches.
    struct Tree {
        watches: Watches,
        mask: WatchMask,
        directories: HashMap<WatchDescriptor, PathBuf>,
    }

    impl Tree {
        /// Watch every directory below `top` (not `top` itself), not following links. A
        /// directory that goes away meanwhile is skipped; the first other failure is the
        /// error, and the rest of the tree is still watched.
        fn add_below(&mut self, top: &Path) -> std::io::Result<()> {
            let mut failed = None;
            let mut pending = vec![top.to_path_buf()];
            while let Some(directory) = pending.pop() {
                let entries = match std::fs::read_dir(&directory) {
                    Ok(entries) => entries,
                    Err(error) if error.kind() == ErrorKind::NotFound => continue,
                    Err(error) => {
                        failed.get_or_insert(error);
                        continue;
                    }
                };
                for entry in entries.flatten() {
                    if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                        continue;
                    }
                    let path = entry.path();
                    match self.watches.add(&path, self.mask | WatchMask::DONT_FOLLOW) {
                        Ok(descriptor) => {
                            self.directories.insert(descriptor, path.clone());
                            pending.push(path);
                        }
                        Err(error) if error.kind() == ErrorKind::NotFound => {}
                        Err(error) => {
                            failed.get_or_insert(error);
                        }
                    }
                }
            }
            failed.map_or(Ok(()), Err)
        }

        /// Watch `directory`, a new entry, and every directory below it.
        fn add(&mut self, directory: PathBuf) {
            // A directory the watch cannot reach is not reported later; the change that
            // brought it is reported now, and a reader re-reads what it can.
            if let Ok(descriptor) = self
                .watches
                .add(&directory, self.mask | WatchMask::DONT_FOLLOW)
            {
                let _ = self.add_below(&directory);
                self.directories.insert(descriptor, directory);
            }
        }

        /// Stop watching `directory` and what is below it: moved out of the tree, or moved
        /// within it, which [`Self::add`] watches again under its new path.
        fn forget(&mut self, directory: &Path) {
            let gone: Vec<WatchDescriptor> = self
                .directories
                .iter()
                .filter(|(_, path)| path.starts_with(directory))
                .map(|(descriptor, _)| descriptor.clone())
                .collect();
            for descriptor in gone {
                self.directories.remove(&descriptor);
                let _ = self.watches.remove(descriptor);
            }
        }
    }

    impl DirectoryWatch {
        pub(super) fn start(
            directory: &Path,
            depth: Depth,
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
            // The directory itself moving away takes its entries from the path watched.
            let descriptor = watches.add(directory, mask | WatchMask::MOVE_SELF)?;
            let root = directory.to_owned();
            let mut tree = Tree {
                watches: watches.clone(),
                mask,
                directories: HashMap::from([(descriptor.clone(), root.clone())]),
            };
            if depth == Depth::Tree {
                tree.add_below(&root)?;
            }
            let stopped = Arc::new(AtomicBool::new(false));
            let stop = Arc::clone(&stopped);
            let top = descriptor.clone();
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
                        let mut overflowed = false;
                        for event in events {
                            if event.mask.contains(EventMask::IGNORED) {
                                if event.wd == top {
                                    // Dropped, or the directory itself went away, and its
                                    // entries with it: a reader must look again.
                                    if !stop.load(Ordering::Acquire) {
                                        on_change();
                                    }
                                    return;
                                }
                                tree.directories.remove(&event.wd);
                                continue;
                            }
                            if event.wd == top && event.mask.contains(EventMask::MOVE_SELF) {
                                // Ended like a removal: its `IN_IGNORED` reports the change.
                                let _ = tree.watches.remove(top.clone());
                                continue;
                            }
                            // An overflowed queue may have hidden a change; re-reading is
                            // cheap.
                            overflowed |= event.mask.contains(EventMask::Q_OVERFLOW);
                            let (Some(name), Some(parent)) =
                                (event.name, tree.directories.get(&event.wd))
                            else {
                                continue;
                            };
                            let path = parent.join(name);
                            changed |= names(&path);
                            if depth == Depth::Tree && event.mask.contains(EventMask::ISDIR) {
                                if event.mask.contains(EventMask::MOVED_FROM) {
                                    tree.forget(&path);
                                } else if event
                                    .mask
                                    .intersects(EventMask::CREATE | EventMask::MOVED_TO)
                                {
                                    // Watched before the change is reported, so an entry
                                    // written into it after the reader looks is reported too.
                                    tree.add(path);
                                }
                            }
                        }
                        if overflowed && depth == Depth::Tree {
                            // A directory created meanwhile may be unwatched.
                            let _ = tree.add_below(&root);
                        }
                        if (changed || overflowed) && !stop.load(Ordering::Acquire) {
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

    #[test]
    fn a_tree_watch_reports_writes_in_subdirectories_old_and_new() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let nested = dir.path().join("package").join("payload");
        std::fs::create_dir_all(&nested).expect("nested directory");
        std::fs::write(nested.join("manifest"), b"one").expect("write");
        let (count, on_change) = counted();
        let _watch = watch_tree_changes(dir.path(), |_| true, on_change).expect("watch");
        std::fs::write(nested.join("manifest"), b"two").expect("rewrite");
        assert!(reaches(&count, 1), "a write two levels down is a change");

        let later = dir.path().join("later").join("deeper");
        std::fs::create_dir_all(&later).expect("new nested directory");
        assert!(reaches(&count, 2), "a new subdirectory is a change");
        // Let the watch take the new directories in before writing into them.
        std::thread::sleep(QUIET);
        let seen = count.load(Ordering::SeqCst);
        std::fs::write(later.join("manifest"), b"one").expect("write in new directory");
        assert!(
            reaches(&count, seen + 1),
            "a write in a directory created after the watch started is a change"
        );
    }

    #[test]
    fn a_tree_watch_follows_a_directory_moved_within_it() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let before = dir.path().join("before");
        std::fs::create_dir_all(before.join("inner")).expect("directory");
        let changed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = Arc::clone(&changed);
        let (count, on_change) = counted();
        let _watch = watch_tree_changes(
            dir.path(),
            move |path| {
                record.lock().expect("paths").push(path.to_path_buf());
                true
            },
            on_change,
        )
        .expect("watch");
        let after = dir.path().join("after");
        std::fs::rename(&before, &after).expect("move");
        assert!(reaches(&count, 1), "a moved directory is a change");
        std::thread::sleep(QUIET);
        let seen = count.load(Ordering::SeqCst);
        std::fs::write(after.join("inner").join("file"), b"").expect("write");
        assert!(reaches(&count, seen + 1), "the moved tree is still watched");
        assert!(
            changed
                .lock()
                .expect("paths")
                .contains(&after.join("inner").join("file")),
            "an entry is named by its new path"
        );
    }

    #[test]
    fn reading_an_entry_is_not_a_change() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let file = dir.path().join("value");
        std::fs::write(&file, b"one").expect("write");
        let (count, on_change) = counted();
        let _watch = watch_changes(dir.path(), |_| true, on_change).expect("watch");
        for _ in 0..20 {
            std::fs::read(&file).expect("read");
            // Opened for writing, as a lock file is, and closed unchanged.
            drop(
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&file)
                    .expect("open"),
            );
        }
        std::thread::sleep(QUIET);
        assert_eq!(count.load(Ordering::SeqCst), 0, "a read wakes nothing");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_directory_going_away_is_a_change() {
        let parent = tempfile::tempdir().expect("temporary directory");
        let dir = parent.path().join("watched");
        std::fs::create_dir(&dir).expect("directory");
        let (count, on_change) = counted();
        let _watch = watch_changes(&dir, |_| true, on_change).expect("watch");
        std::fs::remove_dir(&dir).expect("remove");
        assert!(reaches(&count, 1), "a removed directory is a change");

        std::fs::create_dir(&dir).expect("directory");
        let (count, on_change) = counted();
        let _watch = watch_changes(&dir, |_| true, on_change).expect("watch");
        std::fs::rename(&dir, parent.path().join("elsewhere")).expect("move");
        assert!(reaches(&count, 1), "a directory moved away is a change");
    }

    #[test]
    fn an_entries_watch_does_not_reach_into_subdirectories() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).expect("directory");
        let (count, on_change) = counted();
        let _watch = watch_changes(dir.path(), |_| true, on_change).expect("watch");
        std::fs::write(nested.join("file"), b"").expect("write");
        std::thread::sleep(QUIET);
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "a nested write is not seen"
        );
    }
}
