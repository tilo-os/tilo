//! Watching a native directory, or a whole tree, for changes: the one watch
//! behind [`Dir::watch`](crate::Dir::watch), [`super::watch_entries`] and
//! the callbacks of [`watch_changes`] and its siblings.
//!
//! A [`Watcher`] reports, on a thread of its own, which paths changed and
//! what each is now ([`Event`]). A reader that only cares that something
//! changed asks for [`watch_changes`] (or [`watch_tree_changes`],
//! [`watch_presence`]): the same watch, with the paths it does not care
//! about dropped and the rest folded into one call per batch.
//!
//! On Linux the watch asks inotify for changes only: writes, creation,
//! removal and renames, and the attributes of the watched directories
//! themselves (where a FUSE filesystem raises
//! [`super::CHANGE_SIGNAL_XATTR`]). Opening and reading a file are left out,
//! and so are a file's attributes. The `notify` crate's inotify watch always
//! includes opens, so its thread woke for every read of any file in the
//! watched directory, the watching process's own included. Elsewhere
//! `notify` reports the platform's events, and its access events are
//! dropped.
//!
//! A reader that only cares which entries exist (a socket that is to appear,
//! a device node, the directories on the way to a file) asks for
//! [`Watcher::presence`], which leaves writes out too. Writes to an entry of
//! `/dev` (every reply of a FUSE server to `/dev/fuse`, every write to
//! `/dev/null` or `/dev/ptmx`) or of a home folder are frequent, and each one
//! would otherwise wake the watch's thread for nothing.
//!
//! A tree watch ([`Watcher::tree`]) watches every directory below the root
//! as well, a subdirectory created or moved in from then on as soon as it is
//! reported, without following links; [`Watcher::skip`] leaves chosen
//! subdirectories (a build's output) out.

use std::path::{Path, PathBuf};

use crate::watch::ChangeKind;

/// The name of a watch's thread (Linux), as it shows in a wakeup report.
pub const THREAD_NAME: &str = "tilo-dir-watch";

/// The name of the thread that reads the process's one inotify instance and
/// hands each watch its changes (Linux).
pub const HUB_THREAD_NAME: &str = "tilo-inotify";

/// One change a [`Watcher`] reports.
///
/// `kind` is read from the filesystem when the change is delivered, not from
/// the platform's event. Events arrive after the fact, and under load long
/// after: by the time a write's rename is delivered the file may already be
/// removed again. Mixing the two views reorders history: a rename looked up
/// late says `Removed`, and the next event of the same burst, taken at its
/// word, says `Changed` after it, for a file that is gone. Read at delivery,
/// each change is the path's state at a later moment than the one before it,
/// and any change after the last look raises another event, so the last
/// change reported for a path is what the path is.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Event {
    /// The changed entry's full path; for [`ChangeKind::Rescan`], and for
    /// the end of the watch, the watched path.
    pub path: PathBuf,
    /// [`ChangeKind::Created`] for an entry that is new or moved in and
    /// still there, [`ChangeKind::Changed`] for one written or otherwise
    /// still there, [`ChangeKind::Removed`] for one gone, and for the watched
    /// path itself removed or moved away (the watch ends then);
    /// [`ChangeKind::Rescan`] when changes may have been missed (an
    /// overflowed or failed queue).
    pub kind: ChangeKind,
}

/// A live watch; dropping it stops notifications.
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

type Skip = Box<dyn Fn(&Path) -> bool + Send>;

/// A watch to start on a native directory (or a file, for
/// [`Watcher::entries`]): see the module docs.
#[must_use = "a watcher watches nothing until it is started"]
pub struct Watcher {
    root: PathBuf,
    depth: Depth,
    reported: Reported,
    skip: Skip,
}

impl Watcher {
    /// Watch the entries directly in `dir`, not what happens below its
    /// subdirectories. A file is watched for writes to it.
    pub fn entries(dir: impl Into<PathBuf>) -> Self {
        Self::new(dir.into(), Depth::Entries)
    }

    /// Watch every entry below `root`, at any depth: an entry of a
    /// subdirectory, one created or moved in later included.
    pub fn tree(root: impl Into<PathBuf>) -> Self {
        Self::new(root.into(), Depth::Tree)
    }

    fn new(root: PathBuf, depth: Depth) -> Self {
        Self {
            root,
            depth,
            reported: Reported::Changes,
            skip: Box::new(|_| false),
        }
    }

    /// Report only entries created, removed or renamed, not writes to them.
    pub fn presence(mut self) -> Self {
        self.reported = Reported::Entries;
        self
    }

    /// Leave the subdirectories `skip` accepts (given their full path), and
    /// everything below them, out of a tree watch. Their own creation and
    /// removal are still reported.
    pub fn skip(mut self, skip: impl Fn(&Path) -> bool + Send + 'static) -> Self {
        self.skip = Box::new(skip);
        self
    }

    /// Start watching: `on_events` is called on the watch's thread with each
    /// batch of changes the platform delivers together, in order.
    ///
    /// # Errors
    ///
    /// The platform's failure to watch the root or a directory below it.
    pub fn start(
        self,
        on_events: impl FnMut(&[Event]) + Send + 'static,
    ) -> std::io::Result<ChangeWatch> {
        #[cfg(target_os = "linux")]
        {
            Ok(ChangeWatch {
                _watch: inotify_watch::DirectoryWatch::start(self, on_events)?,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(ChangeWatch {
                _watcher: notify_watch::start(self, on_events)?,
            })
        }
    }

    /// Start watching, calling `on_change` once for each batch of changes in
    /// which `names` accepts a changed entry's full path, the watched path
    /// itself went away (the watch ends then), or changes may have been
    /// missed.
    ///
    /// # Errors
    ///
    /// As [`Self::start`].
    pub fn on_change(
        self,
        names: impl Fn(&Path) -> bool + Send + 'static,
        on_change: impl Fn() + Send + 'static,
    ) -> std::io::Result<ChangeWatch> {
        let root = self.root.clone();
        self.start(move |events| {
            let seen = events.iter().any(|event| {
                event.kind == ChangeKind::Rescan
                    || (event.path == root && event.kind == ChangeKind::Removed)
                    || names(&event.path)
            });
            if seen {
                on_change();
            }
        })
    }
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
    Watcher::entries(directory).on_change(names, on_change)
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
    Watcher::tree(root).on_change(names, on_change)
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
    Watcher::entries(directory)
        .presence()
        .on_change(names, on_change)
}

/// What `path` is now, given whether the platform said it was created.
/// A dangling symlink is still an entry, and a name that cannot be looked up
/// for any other reason may have changed.
fn event_now(path: PathBuf, created: bool) -> Event {
    let kind = match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ChangeKind::Removed,
        _ if created => ChangeKind::Created,
        _ => ChangeKind::Changed,
    };
    Event { path, kind }
}

/// `native::atomic`'s temporary names: `.<name>.<pid>.<sequence>.tmp`. A
/// write's hidden temporary sibling is left out; the rename that publishes
/// it reports the real name.
fn is_temporary(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
        && path.extension() == Some(std::ffi::OsStr::new("tmp"))
}

#[cfg(target_os = "linux")]
mod inotify_watch {
    use super::{
        Depth, Event, HUB_THREAD_NAME, Reported, Skip, THREAD_NAME, Watcher, event_now,
        is_temporary,
    };
    use crate::watch::ChangeKind;
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, mpsc};

    use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask, Watches};
    use tilo_lock::MutexRecover as _;

    /// Creation, removal and renames of the directory's entries.
    const ENTRIES: WatchMask = WatchMask::CREATE
        .union(WatchMask::DELETE)
        .union(WatchMask::MOVED_FROM)
        .union(WatchMask::MOVED_TO);

    /// Writes to the directory's entries besides [`ENTRIES`], and changes of
    /// the directory's own attributes (a file's are dropped as they arrive).
    /// Not `IN_CLOSE_WRITE`: every write already reports `IN_MODIFY`, and a
    /// file opened for writing and closed unchanged (a lock file, a store
    /// opened read-write to be read) changed nothing.
    const CHANGES: WatchMask = ENTRIES.union(WatchMask::MODIFY).union(WatchMask::ATTRIB);

    /// One directory, or a tree, watched by a thread of its own, which waits until the
    /// process's one inotify instance ([`hub`]) hands it a change of its directories.
    pub(super) struct DirectoryWatch {
        hub: Arc<Hub>,
        subscriber: u64,
        stopped: Arc<AtomicBool>,
    }

    /// One event as the kernel reported it, owned, handed from the hub's reader to the
    /// watch it is for.
    struct Raw {
        wd: WatchDescriptor,
        mask: EventMask,
        name: Option<OsString>,
    }

    /// What the hub's reader hands a watch's thread.
    enum Delivery {
        Events(Vec<Raw>),
        /// The kernel's queue overflowed: changes may have been missed.
        Overflowed,
        /// The instance failed: nothing more will be reported.
        Failed,
    }

    /// Which watch wants a watched inode's events, and which of them.
    #[derive(Default)]
    struct Interest {
        /// Each watch's subscriber id and the events it asked for.
        subscribers: Vec<(u64, WatchMask)>,
    }

    #[derive(Default)]
    struct Routes {
        by_wd: HashMap<WatchDescriptor, Interest>,
        senders: HashMap<u64, mpsc::Sender<Delivery>>,
        next: u64,
    }

    /// The process's one inotify instance, shared by every watch.
    ///
    /// Each instance counts against the user's `fs.inotify.max_user_instances` (128 by
    /// default), shared by every process of the user: a session whose every watch (the app
    /// registry, the identity, each device and settings directory) opened an instance of its
    /// own took about sixty, and a second session beside a desktop left later watches
    /// failing with "Too many open files". The kernel gives one instance a watch descriptor
    /// per inode, so two watches of one directory share it: the hub keeps who wants which
    /// events of it, adds masks together, and removes the kernel's watch only once nobody
    /// wants it.
    pub(super) struct Hub {
        watches: Watches,
        routes: Mutex<Routes>,
    }

    /// Masks that matter to a subscriber whatever it asked for.
    const ALWAYS: EventMask = EventMask::IGNORED.union(EventMask::Q_OVERFLOW);

    impl Hub {
        fn subscribe(&self, sender: mpsc::Sender<Delivery>) -> u64 {
            let mut routes = self.routes.lock_recover();
            routes.next = routes.next.wrapping_add(1);
            let id = routes.next;
            routes.senders.insert(id, sender);
            drop(routes);
            id
        }

        /// Watch `path` for `subscriber`'s `mask`. The routes stay locked while the kernel
        /// adds the watch, so its first event finds the subscriber.
        fn add(
            &self,
            subscriber: u64,
            path: &Path,
            mask: WatchMask,
        ) -> std::io::Result<WatchDescriptor> {
            let mut routes = self.routes.lock_recover();
            let mut watches = self.watches.clone();
            let wd = watches.add(path, mask | WatchMask::MASK_ADD)?;
            let interest = routes.by_wd.entry(wd.clone()).or_default();
            if let Some(held) = interest
                .subscribers
                .iter_mut()
                .find(|(id, _)| *id == subscriber)
            {
                held.1 |= mask;
            } else {
                interest.subscribers.push((subscriber, mask));
            }
            drop(routes);
            Ok(wd)
        }

        /// `subscriber` no longer wants `wd`; the kernel's watch goes once nobody does.
        fn remove(&self, subscriber: u64, wd: &WatchDescriptor) {
            let mut routes = self.routes.lock_recover();
            let Some(interest) = routes.by_wd.get_mut(wd) else {
                return;
            };
            interest.subscribers.retain(|(id, _)| *id != subscriber);
            if interest.subscribers.is_empty() {
                routes.by_wd.remove(wd);
                let _ = self.watches.clone().remove(wd.clone());
            }
            drop(routes);
        }

        /// `subscriber` is gone: every watch it held is let go.
        fn unsubscribe(&self, subscriber: u64) {
            let mut routes = self.routes.lock_recover();
            routes.senders.remove(&subscriber);
            let mut emptied = Vec::new();
            for (wd, interest) in &mut routes.by_wd {
                interest.subscribers.retain(|(id, _)| *id != subscriber);
                if interest.subscribers.is_empty() {
                    emptied.push(wd.clone());
                }
            }
            for wd in emptied {
                routes.by_wd.remove(&wd);
                let _ = self.watches.clone().remove(wd);
            }
            drop(routes);
        }

        /// Hand one read's events to the watches they are for, each in order.
        fn dispatch(&self, events: inotify::Events<'_>) {
            let mut routes = self.routes.lock_recover();
            let mut batches: HashMap<u64, Vec<Raw>> = HashMap::new();
            let mut overflowed = false;
            for event in events {
                if event.mask.contains(EventMask::Q_OVERFLOW) {
                    overflowed = true;
                    continue;
                }
                let ignored = event.mask.contains(EventMask::IGNORED);
                let Some(interest) = routes.by_wd.get(&event.wd) else {
                    continue;
                };
                for (subscriber, wanted) in &interest.subscribers {
                    let wanted = EventMask::from_bits_truncate(wanted.bits()) | ALWAYS;
                    // ISDIR qualifies an event; it is never one by itself.
                    if !(event.mask & !EventMask::ISDIR).intersects(wanted) {
                        continue;
                    }
                    batches.entry(*subscriber).or_default().push(Raw {
                        wd: event.wd.clone(),
                        mask: event.mask,
                        name: event.name.map(std::ffi::OsStr::to_os_string),
                    });
                }
                if ignored {
                    // The kernel dropped the watch (its inode went away): nobody holds it.
                    routes.by_wd.remove(&event.wd);
                }
            }
            for (subscriber, batch) in batches {
                if let Some(sender) = routes.senders.get(&subscriber) {
                    let _ = sender.send(Delivery::Events(batch));
                }
            }
            if overflowed {
                for sender in routes.senders.values() {
                    let _ = sender.send(Delivery::Overflowed);
                }
            }
        }

        fn fail(&self) {
            let mut routes = self.routes.lock_recover();
            for sender in routes.senders.values() {
                let _ = sender.send(Delivery::Failed);
            }
            routes.senders.clear();
            routes.by_wd.clear();
        }
    }

    /// The process's inotify instance, opened by the first watch and read by one thread of
    /// its own for as long as the process runs. A failed instance is let go, so a later
    /// watch opens a new one; failing to open one is the watch's error.
    fn hub() -> std::io::Result<Arc<Hub>> {
        static HUB: Mutex<Option<Arc<Hub>>> = Mutex::new(None);
        let mut slot = HUB.lock_recover();
        if let Some(hub) = slot.as_ref() {
            return Ok(Arc::clone(hub));
        }
        let mut inotify = Inotify::init()?;
        let hub = Arc::new(Hub {
            watches: inotify.watches(),
            routes: Mutex::new(Routes::default()),
        });
        let reader = Arc::clone(&hub);
        std::thread::Builder::new()
            .name(HUB_THREAD_NAME.to_owned())
            .spawn(move || {
                let mut buffer = [0_u8; 16 * 1024];
                loop {
                    match inotify.read_events_blocking(&mut buffer) {
                        Ok(events) => reader.dispatch(events),
                        Err(error) if error.kind() == ErrorKind::Interrupted => {}
                        Err(_) => {
                            // Let go first, so a watch started from here opens a new instance.
                            HUB.lock_recover().take();
                            reader.fail();
                            return;
                        }
                    }
                }
            })?;
        *slot = Some(Arc::clone(&hub));
        drop(slot);
        Ok(hub)
    }

    /// The directories of a tree watch, by their watches.
    struct Tree {
        hub: Arc<Hub>,
        subscriber: u64,
        mask: WatchMask,
        skip: Skip,
        directories: HashMap<WatchDescriptor, PathBuf>,
    }

    impl Tree {
        fn watch(&self, path: &Path) -> std::io::Result<WatchDescriptor> {
            self.hub
                .add(self.subscriber, path, self.mask | WatchMask::DONT_FOLLOW)
        }

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
                    if (self.skip)(&path) {
                        continue;
                    }
                    match self.watch(&path) {
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
            if (self.skip)(&directory) {
                return;
            }
            // A directory the watch cannot reach is not reported later; the change that
            // brought it is reported now, and a reader re-reads what it can.
            if let Ok(descriptor) = self.watch(&directory) {
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
                self.hub.remove(self.subscriber, &descriptor);
            }
        }
    }

    /// What one inotify event means for a watch's thread.
    enum Step {
        Next,
        /// The watched path went away.
        Ended,
    }

    /// The thread's view of a watch: its directories and where it started.
    struct Reader {
        tree: Tree,
        root: PathBuf,
        top: WatchDescriptor,
        depth: Depth,
    }

    impl Reader {
        /// Take in one event, adding what it reports to `batch`.
        fn take(&mut self, event: &Raw, batch: &mut Vec<Event>) -> Step {
            if event.mask.contains(EventMask::IGNORED) {
                if event.wd == self.top {
                    // The watched path itself went away, and its entries with it: a reader
                    // must look again.
                    return Step::Ended;
                }
                self.tree.directories.remove(&event.wd);
                return Step::Next;
            }
            if event.wd == self.top && event.mask.contains(EventMask::MOVE_SELF) {
                // Moved away, which ends the watch like a removal. Another watch of the same
                // directory may keep the kernel's watch, so no `IN_IGNORED` need follow.
                return Step::Ended;
            }
            let Some(parent) = self.tree.directories.get(&event.wd) else {
                return Step::Next;
            };
            let Some(name) = &event.name else {
                // The watched directory itself (or a watched file): its attributes,
                // or a write to the file.
                if event.mask.intersects(EventMask::ATTRIB | EventMask::MODIFY) {
                    batch.push(event_now(parent.clone(), false));
                }
                return Step::Next;
            };
            let path = parent.join(name);
            if self.depth == Depth::Tree && event.mask.contains(EventMask::ISDIR) {
                if event.mask.contains(EventMask::MOVED_FROM) {
                    self.tree.forget(&path);
                } else if event
                    .mask
                    .intersects(EventMask::CREATE | EventMask::MOVED_TO)
                {
                    // Watched before the change is reported, so an entry written into
                    // it after the reader looks is reported too.
                    self.tree.add(path.clone());
                }
            }
            // An entry's attributes are not a change of it; a watched directory
            // reports its own.
            if !event.mask.contains(EventMask::ATTRIB) && !is_temporary(&path) {
                let created = event
                    .mask
                    .intersects(EventMask::CREATE | EventMask::MOVED_TO);
                batch.push(event_now(path, created));
            }
            Step::Next
        }

        fn rescan(&mut self, batch: &mut Vec<Event>) {
            if self.depth == Depth::Tree {
                // A directory created meanwhile may be unwatched.
                let _ = self.tree.add_below(&self.root);
            }
            batch.push(Event {
                path: self.root.clone(),
                kind: ChangeKind::Rescan,
            });
        }
    }

    impl DirectoryWatch {
        pub(super) fn start(
            spec: Watcher,
            mut on_events: impl FnMut(&[Event]) + Send + 'static,
        ) -> std::io::Result<Self> {
            let Watcher {
                root,
                depth,
                reported,
                skip,
            } = spec;
            let hub = hub()?;
            let (sender, deliveries) = mpsc::channel();
            let subscriber = hub.subscribe(sender);
            let unsubscribe = |error| {
                hub.unsubscribe(subscriber);
                error
            };
            let mask = match reported {
                Reported::Changes => CHANGES,
                Reported::Entries => ENTRIES,
            };
            // The directory itself moving away takes its entries from the path watched.
            let descriptor = hub
                .add(subscriber, &root, mask | WatchMask::MOVE_SELF)
                .map_err(unsubscribe)?;
            let mut tree = Tree {
                hub: Arc::clone(&hub),
                subscriber,
                mask,
                skip,
                directories: HashMap::from([(descriptor.clone(), root.clone())]),
            };
            let depth = if root.is_dir() { depth } else { Depth::Entries };
            if depth == Depth::Tree {
                tree.add_below(&root).map_err(unsubscribe)?;
            }
            let mut reader = Reader {
                tree,
                root,
                top: descriptor,
                depth,
            };
            let stopped = Arc::new(AtomicBool::new(false));
            let stop = Arc::clone(&stopped);
            let spawned = std::thread::Builder::new()
                .name(THREAD_NAME.to_owned())
                .spawn(move || {
                    let mut batch = Vec::new();
                    // Ends when the watch is dropped: the hub lets go of its sender.
                    while let Ok(delivery) = deliveries.recv() {
                        if stop.load(Ordering::Acquire) {
                            return;
                        }
                        let events = match delivery {
                            Delivery::Events(events) => events,
                            Delivery::Overflowed => {
                                reader.rescan(&mut batch);
                                on_events(&batch);
                                batch.clear();
                                continue;
                            }
                            Delivery::Failed => {
                                reader.rescan(&mut batch);
                                on_events(&batch);
                                return;
                            }
                        };
                        for event in &events {
                            if matches!(reader.take(event, &mut batch), Step::Ended) {
                                batch.push(Event {
                                    path: reader.root.clone(),
                                    kind: ChangeKind::Removed,
                                });
                                if !stop.load(Ordering::Acquire) {
                                    on_events(&batch);
                                }
                                reader.tree.hub.unsubscribe(reader.tree.subscriber);
                                return;
                            }
                        }
                        if !batch.is_empty() && !stop.load(Ordering::Acquire) {
                            on_events(&batch);
                        }
                        batch.clear();
                    }
                });
            if let Err(error) = spawned {
                hub.unsubscribe(subscriber);
                return Err(error);
            }
            Ok(Self {
                hub,
                subscriber,
                stopped,
            })
        }
    }

    impl Drop for DirectoryWatch {
        fn drop(&mut self) {
            self.stopped.store(true, Ordering::Release);
            // Letting go of its sender ends the watch's thread.
            self.hub.unsubscribe(self.subscriber);
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod notify_watch {
    use super::{Depth, Event, Reported, Watcher, event_now, is_temporary};
    use crate::watch::ChangeKind;
    use notify::event::ModifyKind;
    use notify::{EventKind, RecursiveMode, Watcher as _};
    use std::path::Path;

    pub(super) fn start(
        watcher: Watcher,
        mut on_events: impl FnMut(&[Event]) + Send + 'static,
    ) -> std::io::Result<notify::RecommendedWatcher> {
        let Watcher {
            root,
            depth,
            reported,
            skip,
        } = watcher;
        // Taken before the watch starts, so a change made between the two is
        // never mistaken for earlier history.
        #[cfg(target_os = "macos")]
        let started = Started::of(&root);
        let watched = root.clone();
        let skipped = move |path: &Path| {
            path.ancestors()
                .skip(1)
                .take_while(|ancestor| *ancestor != watched)
                .any(|ancestor| skip(ancestor))
        };
        let callback_root = root.clone();
        let handler = move |event: notify::Result<notify::Event>| {
            let root = &callback_root;
            let event = match event {
                Ok(event) if !event.need_rescan() => event,
                // An overflowed or failed queue may have hidden a change.
                _ => {
                    on_events(&[Event {
                        path: root.clone(),
                        kind: ChangeKind::Rescan,
                    }]);
                    return;
                }
            };
            #[cfg(target_os = "macos")]
            if started
                .as_ref()
                .is_some_and(|started| started.explains(&event))
            {
                return;
            }
            let created = match event.kind {
                EventKind::Access(_) => return,
                EventKind::Modify(ModifyKind::Name(_)) => true,
                EventKind::Modify(_) if reported == Reported::Entries => return,
                EventKind::Create(_) => true,
                _ => false,
            };
            let metadata = matches!(event.kind, EventKind::Modify(ModifyKind::Metadata(_)));
            let batch: Vec<Event> = event
                .paths
                .into_iter()
                // A file's attributes are not a change of it; the watched
                // directory reports its own.
                .filter(|path| !metadata || path == root)
                .filter(|path| !is_temporary(path) && !skipped(path))
                .map(|path| event_now(path, created))
                .collect();
            if !batch.is_empty() {
                on_events(&batch);
            }
        };
        // Links are not followed, as on Linux: a linked folder is not part of
        // the tree.
        let config = notify::Config::default().with_follow_symlinks(false);
        let mut native = notify::RecommendedWatcher::new(handler, config).map_err(error)?;
        let mode = match depth {
            Depth::Entries => RecursiveMode::NonRecursive,
            Depth::Tree => RecursiveMode::Recursive,
        };
        native.watch(&root, mode).map_err(error)?;
        Ok(native)
    }

    fn error(error: notify::Error) -> std::io::Error {
        match error.kind {
            notify::ErrorKind::PathNotFound => std::io::ErrorKind::NotFound.into(),
            notify::ErrorKind::Io(io) => io,
            _ => std::io::Error::other(error.to_string()),
        }
    }

    /// The watched directory as it was when its watch started.
    ///
    /// `FSEvents` numbers an event when its daemon gets to it, not when the
    /// change was made, so a stream started "from now" can still be handed
    /// what happened to the watched directory just before: its own creation
    /// above all, when it was made a moment earlier. Reported, that history
    /// would wake the watch's owner for a change it has read already, since
    /// whoever starts a watch reads the directory once after it.
    ///
    /// So an event on the directory itself is dropped while the directory is
    /// still the one that was watched, with the mode and owner it had then.
    /// A removal, a replacement or a new mode or owner is reported. Changes
    /// to its entries move its times too, but are reported by their own
    /// names, so the times are not compared: a late report of the
    /// directory's making must not pass as a change because an entry was
    /// created since. The one change to the directory itself left unreported
    /// is to its extended attributes, which only a Linux FUSE mount uses as a
    /// signal ([`crate::native::CHANGE_SIGNAL_XATTR`]), and inotify reports
    /// only what happens after its watch is added.
    #[cfg(target_os = "macos")]
    struct Started {
        /// The directory as watched.
        dir: std::path::PathBuf,
        /// The same, canonical, as `FSEvents` names it.
        canonical: std::path::PathBuf,
        identity: Identity,
    }

    #[cfg(target_os = "macos")]
    impl Started {
        fn of(dir: &Path) -> Option<Self> {
            Some(Self {
                identity: Identity::of(dir)?,
                dir: dir.to_path_buf(),
                canonical: dir.canonicalize().ok()?,
            })
        }

        /// Whether `event` reports only what the directory went through
        /// before its watch started.
        fn explains(&self, event: &notify::Event) -> bool {
            !event.paths.is_empty()
                && event
                    .paths
                    .iter()
                    .all(|path| *path == self.dir || *path == self.canonical)
                && Identity::of(&self.dir).as_ref() == Some(&self.identity)
        }
    }

    /// Which directory a path names, and who may do what in it.
    #[cfg(target_os = "macos")]
    #[derive(Debug, PartialEq, Eq)]
    struct Identity {
        device: u64,
        inode: u64,
        born: Option<std::time::SystemTime>,
        mode: u32,
        owner: (u32, u32),
    }

    #[cfg(target_os = "macos")]
    impl Identity {
        fn of(dir: &Path) -> Option<Self> {
            use std::os::unix::fs::MetadataExt as _;
            let metadata = std::fs::metadata(dir).ok()?;
            Some(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                born: metadata.created().ok(),
                mode: metadata.mode(),
                owner: (metadata.uid(), metadata.gid()),
            })
        }
    }
}

/// Filesystems whose entries can change without passing through this
/// kernel, by `statfs` magic: FUSE, NFS, SMB, CIFS, SMB2, 9P, Ceph and the
/// two AFS implementations.
#[cfg(target_os = "linux")]
const REMOTE_FILESYSTEMS: [u32; 9] = [
    FUSE_SUPER_MAGIC,
    0x0000_6969,
    0x0000_517B,
    0xFF53_4D42,
    0xFE53_4D42,
    0x0102_1997,
    0x00C3_6400,
    0x6B41_4653,
    0x5346_414F,
];

/// `statfs` magic of FUSE.
#[cfg(target_os = "linux")]
const FUSE_SUPER_MAGIC: u32 = 0x6573_5546;

/// [`crate::native::watch_sees_every_change`].
#[cfg(target_os = "linux")]
pub(crate) fn sees_every_change(dir: &Path) -> bool {
    let Ok(stat) = rustix::fs::statfs(dir) else {
        return false;
    };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "statfs magic numbers are 32-bit; the width and sign of `f_type` vary by target"
    )]
    let magic = stat.f_type as u32;
    if magic == FUSE_SUPER_MAGIC {
        return signals_every_change(dir);
    }
    !REMOTE_FILESYSTEMS.contains(&magic)
}

/// Whether the FUSE filesystem holding `dir` says it tells the kernel of
/// every change, by answering [`crate::native::CHANGE_SIGNAL_XATTR`] there.
#[cfg(target_os = "linux")]
fn signals_every_change(dir: &Path) -> bool {
    let mut value = [0_u8; 8];
    rustix::fs::getxattr(dir, crate::native::CHANGE_SIGNAL_XATTR, &mut value[..])
        .is_ok_and(|len| value.get(..len) == Some(b"1".as_slice()))
}

/// Elsewhere the filesystem's type is not told apart, so a watch is trusted
/// for local edits only.
#[cfg(not(target_os = "linux"))]
pub(crate) fn sees_every_change(_dir: &Path) -> bool {
    false
}

/// [`Dir::watch`](crate::Dir::watch) and [`crate::native::watch_entries`]:
/// the watch of `path` below `root`, its changes queued as paths relative to
/// `root`.
pub(crate) fn stream(
    root: &Path,
    path: &crate::VPath,
    tree: bool,
) -> Result<crate::Watch, crate::FsError> {
    use crate::watch::{Change, Inbox};
    use std::sync::Arc;
    let target = crate::disk::resolve(root, path);
    let spec = if tree {
        Watcher::tree(target)
    } else {
        Watcher::entries(target)
    };
    let inbox = Arc::new(Inbox::default());
    let sink = Arc::clone(&inbox);
    let root = root.to_path_buf();
    // `FSEvents` reports canonical paths (`/private/var/...` for `/var/...`).
    let canonical = root.canonicalize().ok();
    let watched = path.clone();
    let source = spec.start(move |events| {
        for event in events {
            let path = if event.kind == ChangeKind::Rescan {
                Some(watched.clone())
            } else {
                relative(&root, &event.path)
                    .or_else(|| relative(canonical.as_deref()?, &event.path))
            };
            if let Some(path) = path {
                sink.push(Change {
                    path,
                    kind: event.kind,
                });
            }
        }
    })?;
    Ok(crate::Watch::new(inbox, Box::new(source)))
}

/// `path` below `root` as a [`crate::VPath`]; `None` outside it or for a
/// name that is not UTF-8.
fn relative(root: &Path, path: &Path) -> Option<crate::VPath> {
    let parts: Option<Vec<&str>> = path
        .strip_prefix(root)
        .ok()?
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect();
    crate::VPath::new(&parts?.join("/")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::Change;
    use crate::{VPath, Watch};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
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

    /// The events a watcher reports, collected.
    fn recorded(watcher: Watcher) -> (Arc<Mutex<Vec<Event>>>, ChangeWatch) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        let watch = watcher
            .start(move |events| record.lock().expect("events").extend_from_slice(events))
            .expect("watch");
        (seen, watch)
    }

    fn until_seen(seen: &Mutex<Vec<Event>>, what: &str, found: impl Fn(&Event) -> bool) {
        tilo_test_env::wait_until(what, || seen.lock().expect("events").iter().any(&found));
    }

    /// Every watch of a process shares one inotify instance, so watches past
    /// the user's instance limit (128 by default, shared by all the user's
    /// processes) still start, and each still hears its own directory.
    #[cfg(target_os = "linux")]
    #[test]
    fn more_watches_than_the_instance_limit_start_and_each_reports_its_own_changes() {
        let dirs: Vec<_> = (0..200)
            .map(|_| tempfile::tempdir().expect("temporary directory"))
            .collect();
        let counts: Vec<_> = dirs.iter().map(|_| counted()).collect();
        let (counts, callbacks): (Vec<_>, Vec<_>) = counts.into_iter().unzip();
        let _watches: Vec<_> = dirs
            .iter()
            .zip(callbacks)
            .map(|(dir, on_change)| watch_changes(dir.path(), |_| true, on_change).expect("watch"))
            .collect();
        std::fs::write(dirs[0].path().join("first"), b"x").expect("write");
        std::fs::write(dirs[199].path().join("last"), b"x").expect("write");
        assert!(reaches(&counts[0], 1) && reaches(&counts[199], 1));
        std::thread::sleep(QUIET);
        assert_eq!(
            counts[100].load(Ordering::SeqCst),
            0,
            "a watch hears only its own directory"
        );
    }

    /// Two watches of one directory share the kernel's watch of it: each
    /// hears only what it asked for, and dropping one leaves the other.
    #[test]
    fn two_watches_of_one_directory_are_independent() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let file = dir.path().join("value");
        std::fs::write(&file, b"one").expect("write");
        let (presence, on_presence) = counted();
        let presence_watch =
            watch_presence(dir.path(), |_| true, on_presence).expect("presence watch");
        let (changes, on_change) = counted();
        let _changes_watch = watch_changes(dir.path(), |_| true, on_change).expect("watch");
        std::fs::write(&file, b"two").expect("rewrite");
        assert!(reaches(&changes, 1), "the changes watch hears the write");
        std::thread::sleep(QUIET);
        assert_eq!(
            presence.load(Ordering::SeqCst),
            0,
            "the presence watch does not hear a write"
        );
        drop(presence_watch);
        let before = changes.load(Ordering::SeqCst);
        std::fs::write(dir.path().join("new"), b"x").expect("create");
        assert!(
            reaches(&changes, before + 1),
            "dropping the other watch leaves this one watching"
        );
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
        let (seen, _watch) = recorded(Watcher::tree(dir.path()));
        let after = dir.path().join("after");
        std::fs::rename(&before, &after).expect("move");
        until_seen(&seen, "the moved directory", |event| event.path == after);
        std::thread::sleep(QUIET);
        let file = after.join("inner").join("file");
        std::fs::write(&file, b"").expect("write");
        until_seen(&seen, "the entry by its new path", |event| {
            event.path == file && event.kind == ChangeKind::Created
        });
    }

    #[test]
    fn a_tree_watch_reports_paths_and_kinds_and_leaves_skipped_folders_out() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let skipped = dir.path().join("target");
        std::fs::create_dir_all(skipped.join("debug")).expect("skipped directory");
        let (seen, _watch) = recorded(
            Watcher::tree(dir.path()).skip(|path| path.file_name() == Some("target".as_ref())),
        );
        std::fs::write(skipped.join("debug").join("out"), b"").expect("write in skipped");
        let file = dir.path().join("src").join("main.rs");
        std::fs::create_dir(dir.path().join("src")).expect("directory");
        std::thread::sleep(QUIET);
        std::fs::write(&file, b"one").expect("create");
        until_seen(&seen, "the created file", |event| {
            event.path == file && event.kind == ChangeKind::Created
        });
        std::fs::write(&file, b"two").expect("rewrite");
        until_seen(&seen, "the written file", |event| {
            event.path == file && event.kind == ChangeKind::Changed
        });
        std::fs::remove_file(&file).expect("remove");
        until_seen(&seen, "the removed file", |event| {
            event.path == file && event.kind == ChangeKind::Removed
        });
        assert!(
            seen.lock()
                .expect("events")
                .iter()
                .all(|event| !event.path.starts_with(&skipped) || event.path == skipped),
            "nothing below a skipped folder is reported"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_tree_watch_does_not_follow_links() {
        let outside = tempfile::tempdir().expect("temporary directory");
        let dir = tempfile::tempdir().expect("temporary directory");
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).expect("symlink");
        let (count, on_change) = counted();
        let _watch = watch_tree_changes(dir.path(), |_| true, on_change).expect("watch");
        std::fs::write(outside.path().join("file"), b"").expect("write");
        std::thread::sleep(QUIET);
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "a linked folder is not watched"
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
        let _watch = watch_changes(&dir, |_| false, on_change).expect("watch");
        std::fs::remove_dir(&dir).expect("remove");
        assert!(reaches(&count, 1), "a removed directory is a change");

        std::fs::create_dir(&dir).expect("directory");
        let (seen, _watch) = recorded(Watcher::entries(&dir));
        std::fs::rename(&dir, parent.path().join("elsewhere")).expect("move");
        until_seen(&seen, "the end of the watch", |event| {
            event.path == dir && event.kind == ChangeKind::Removed
        });
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

    /// Two atomic writes and a removal, all done before the watch's thread
    /// got to them, as happens under load: each change is what its path is
    /// when delivered, so the file is never reported changed after it is
    /// gone, and its temporaries are not reported at all.
    #[test]
    fn a_file_removed_before_its_writes_are_delivered_is_never_reported_changed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.md");
        let temporary = dir.path().join(".a.md.42.0.tmp");
        assert!(is_temporary(&temporary));
        assert!(!is_temporary(&file));
        assert_eq!(event_now(file.clone(), true).kind, ChangeKind::Removed);
        std::fs::write(&file, b"back").unwrap();
        assert_eq!(event_now(file.clone(), true).kind, ChangeKind::Created);
        assert_eq!(event_now(file, false).kind, ChangeKind::Changed);
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_is_an_entry() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path().join("missing"), &link).unwrap();
        assert_eq!(event_now(link, false).kind, ChangeKind::Changed);
        assert_eq!(
            event_now(dir.path().join("missing"), false).kind,
            ChangeKind::Removed
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_watch_on_a_local_directory_sees_every_change() {
        let dir = tempfile::tempdir().unwrap();
        assert!(sees_every_change(dir.path()));
        assert!(!sees_every_change(&dir.path().join("missing")));
    }

    fn entries(dir: &Path) -> Watch {
        stream(dir, &VPath::root(), false).unwrap()
    }

    /// Every change `watch` reports up to and including the creation of
    /// `name` in `dir`, which it makes. A watch reports in the order the
    /// changes were made, so anything left over from before the watch began
    /// is among them.
    fn changes_until_made(watch: &Watch, dir: &Path, name: &str) -> Vec<Change> {
        std::fs::create_dir(dir.join(name)).unwrap();
        let made = VPath::new(name).unwrap();
        let mut seen = Vec::new();
        tilo_test_env::wait_until("the watch to report the made entry", || {
            while let Some(change) = watch.try_next() {
                seen.push(change);
            }
            seen.iter().any(|change| change.path == made)
        });
        seen
    }

    /// `FSEvents` can hand a watch the creation of the directory it watches
    /// when the directory was made a moment before: the event is numbered
    /// when its daemon gets to it. That is no change since the watch began.
    #[test]
    fn a_watch_on_a_just_made_directory_reports_nothing_of_its_making() {
        for _ in 0..50 {
            let dir = tempfile::tempdir().unwrap();
            let watch = entries(dir.path());
            let seen = changes_until_made(&watch, dir.path(), "made");
            assert!(seen.iter().all(|change| !change.path.is_root()), "{seen:?}");
        }
    }

    /// What the directory itself goes through after the watch began is
    /// reported, as a change of the watched path.
    #[cfg(unix)]
    #[test]
    fn a_change_to_the_watched_directory_itself_is_reported() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let watch = entries(dir.path());
        // Past any report of the directory's making.
        let _ = changes_until_made(&watch, dir.path(), "made");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o750)).unwrap();
        tilo_test_env::wait_until("the watch to report the directory's new mode", || {
            std::iter::from_fn(|| watch.try_next()).any(|change| change.path.is_root())
        });
    }

    #[test]
    fn an_entries_watch_sees_replaced_files_but_not_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let watch = entries(dir.path());
        crate::native::atomic::write(&dir.path().join("sub/inner.json"), b"below").unwrap();
        crate::native::atomic::write(&dir.path().join("top.json"), b"one").unwrap();
        crate::native::atomic::write(&dir.path().join("top.json"), b"two").unwrap();
        let top = VPath::new("top.json").unwrap();
        let mut seen = Vec::new();
        tilo_test_env::wait_until("the replaced file to be watched", || {
            while let Some(change) = watch.try_next() {
                seen.push(change);
            }
            seen.iter().any(|change| change.path == top)
        });
        assert!(
            seen.iter().any(|change| change.path == top
                && matches!(change.kind, ChangeKind::Changed | ChangeKind::Created)),
            "{seen:?}"
        );
        assert!(
            seen.iter()
                .all(|change| !change.path.as_str().starts_with("sub/")
                    && !is_temporary(Path::new(change.path.as_str()))),
            "{seen:?}"
        );
    }
}
