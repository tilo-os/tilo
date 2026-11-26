//! Watching a native directory, or a whole tree, for changes: the one watch behind
//! [`Dir::watch`](crate::Dir::watch) and [`super::watch_entries`]. Linux watches
//! through one inotify instance for the process, macOS through `FSEvents`.

use std::path::{Path, PathBuf};

use crate::watch::ChangeKind;

/// The name of a watch's thread (Linux), as it shows in a wakeup report.
pub const THREAD_NAME: &str = "tilo-dir-watch";

#[cfg(target_os = "linux")]
pub const HUB_THREAD_NAME: &str = "tilo-inotify";

/// One change a [`Watcher`] reports. `kind` is read from the filesystem when the change is delivered,
/// not from the platform's event.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Event {
    /// The changed entry's full path; for [`ChangeKind::Rescan`] and the end of the watch, the watched
    /// path.
    pub path: PathBuf,
    /// [`ChangeKind::Created`] for an entry that is new or moved in and still there,
    /// [`ChangeKind::Changed`] for one written or otherwise changed.
    pub kind: ChangeKind,
}

/// Dropping it stops notifications.
pub struct ChangeWatch {
    #[cfg(target_os = "linux")]
    _watch: inotify_watch::DirectoryWatch,
    #[cfg(not(target_os = "linux"))]
    _watcher: notify::RecommendedWatcher,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Depth {
    /// The directory's own entries.
    Entries,
    Tree,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reported {
    Changes,
    Entries,
    /// [`Self::Changes`], and a writer closing an entry it held open for writing
    /// ([`Watcher::released`]).
    Released,
}

type Skip = Box<dyn Fn(&Path) -> bool + Send>;

/// A watch to start on a native directory (or a file, for [`Watcher::entries`]): see the module docs.
#[must_use = "a watcher watches nothing until it is started"]
pub struct Watcher {
    root: PathBuf,
    depth: Depth,
    reported: Reported,
    skip: Skip,
}

impl Watcher {
    pub fn entries(dir: impl Into<PathBuf>) -> Self {
        Self::new(dir.into(), Depth::Entries)
    }

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

    pub fn presence(mut self) -> Self {
        self.reported = Reported::Entries;
        self
    }

    /// Also reports a writer letting go of an entry: its writable descriptor closed, which is when a
    /// file it stood behind is final.
    pub fn released(mut self) -> Self {
        self.reported = Reported::Released;
        self
    }

    /// Leaves the subdirectories `skip` accepts, and everything below them, out of a tree watch.
    pub fn skip(mut self, skip: impl Fn(&Path) -> bool + Send + 'static) -> Self {
        self.skip = Box::new(skip);
        self
    }

    /// `on_events` runs on the watch's thread with each batch of changes the platform delivers
    /// together, in order.
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

    /// Calls `on_change` once for each batch in which `names` accepts a changed entry's full path.
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

pub fn watch_changes(
    directory: &Path,
    names: impl Fn(&Path) -> bool + Send + 'static,
    on_change: impl Fn() + Send + 'static,
) -> std::io::Result<ChangeWatch> {
    Watcher::entries(directory).on_change(names, on_change)
}

pub fn watch_tree_changes(
    root: &Path,
    names: impl Fn(&Path) -> bool + Send + 'static,
    on_change: impl Fn() + Send + 'static,
) -> std::io::Result<ChangeWatch> {
    Watcher::tree(root).on_change(names, on_change)
}

pub fn watch_presence(
    directory: &Path,
    names: impl Fn(&Path) -> bool + Send + 'static,
    on_change: impl Fn() + Send + 'static,
) -> std::io::Result<ChangeWatch> {
    Watcher::entries(directory)
        .presence()
        .on_change(names, on_change)
}

/// What `path` is now, given whether the platform said it was created; a dangling symlink is still an
/// entry.
fn event_now(path: PathBuf, created: bool) -> Event {
    let kind = match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ChangeKind::Removed,
        _ if created => ChangeKind::Created,
        _ => ChangeKind::Changed,
    };
    Event { path, kind }
}

/// A write's hidden temporary sibling is left out; the rename that publishes it is reported.
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

    /// Writes to the directory's entries besides [`ENTRIES`], and changes of the directory's own
    /// attributes (a file's are dropped as they arrive). Not `IN_CLOSE_WRITE`: every write already
    /// reports `IN_MODIFY`, and a file opened for writing and closed unchanged (a lock file, a store
    /// opened read-write to be read) changed nothing.
    const CHANGES: WatchMask = ENTRIES.union(WatchMask::MODIFY).union(WatchMask::ATTRIB);

    /// [`CHANGES`], and a writer closing a file it had opened for writing: what a file its writer
    /// stands behind ([`super::super::held`]) is worth ends with that close, though the name and its
    /// bytes stay.
    const RELEASES: WatchMask = CHANGES.union(WatchMask::CLOSE_WRITE);

    /// One directory or tree, watched by a thread that waits until the process's one inotify instance
    /// hands it events.
    pub(super) struct DirectoryWatch {
        hub: Arc<Hub>,
        subscriber: u64,
        stopped: Arc<AtomicBool>,
    }

    struct Raw {
        wd: WatchDescriptor,
        mask: EventMask,
        name: Option<OsString>,
    }

    enum Delivery {
        Events(Vec<Raw>),
        /// The kernel's queue overflowed: changes may have been missed.
        Overflowed,
        /// The instance failed: nothing more will be reported.
        Failed,
    }

    #[derive(Default)]
    struct Interest {
        subscribers: Vec<(u64, WatchMask)>,
    }

    #[derive(Default)]
    struct Routes {
        by_wd: HashMap<WatchDescriptor, Interest>,
        senders: HashMap<u64, mpsc::Sender<Delivery>>,
        next: u64,
    }

    /// The process's one inotify instance, shared by every watch: each instance counts against the
    /// user's `fs.inotify.max_user_instances`.
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

        /// Watches `path` for `subscriber`'s `mask`. The routes stay locked while the kernel adds the
        /// watch, so its first event finds the subscriber registered.
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

    /// Opened by the first watch and read by a thread of its own for as long as the process runs.
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

        /// Every directory below `top`, not `top` itself, without following links; a directory that
        /// goes away meanwhile is skipped. Each entry found is added to `found` where it is given.
        fn add_below(
            &mut self,
            top: &Path,
            mut found: Option<&mut Vec<Event>>,
        ) -> std::io::Result<()> {
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
                    let path = entry.path();
                    if let Some(found) = found.as_deref_mut()
                        && !is_temporary(&path)
                    {
                        found.push(event_now(path.clone(), true));
                    }
                    if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                        continue;
                    }
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

        /// Watch a directory that came into the tree, and report what was written into it before
        /// the watch was in place: a writer does not wait for the watch to take a new directory in.
        fn add(&mut self, directory: PathBuf, batch: &mut Vec<Event>) {
            if (self.skip)(&directory) {
                return;
            }
            // A directory the watch cannot reach is not reported later; the change that brought it is
            // reported now, and a reader re-reads what it can.
            if let Ok(descriptor) = self.watch(&directory) {
                let _ = self.add_below(&directory, Some(batch));
                self.directories.insert(descriptor, directory);
            }
        }

        /// Moved out of the tree, or moved within it, which [`Self::add`] watches again.
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

    enum Step {
        Next,
        Ended,
    }

    struct Reader {
        tree: Tree,
        root: PathBuf,
        top: WatchDescriptor,
        depth: Depth,
    }

    impl Reader {
        fn take(&mut self, event: &Raw, batch: &mut Vec<Event>) -> Step {
            if event.mask.contains(EventMask::IGNORED) {
                if event.wd == self.top {
                    // The watched path itself went away, and its entries with it: a reader must look
                    // again.
                    return Step::Ended;
                }
                self.tree.directories.remove(&event.wd);
                return Step::Next;
            }
            if event.wd == self.top && event.mask.contains(EventMask::MOVE_SELF) {
                // Moved away, which ends the watch like a removal. Another watch of the same directory
                // may keep the kernel's watch, so no `IN_IGNORED` need follow.
                return Step::Ended;
            }
            let Some(parent) = self.tree.directories.get(&event.wd) else {
                return Step::Next;
            };
            let Some(name) = &event.name else {
                // The watched directory itself (or a watched file): its attributes, a write or a
                // writer's close of the file.
                if event
                    .mask
                    .intersects(EventMask::ATTRIB | EventMask::MODIFY | EventMask::CLOSE_WRITE)
                {
                    batch.push(event_now(parent.clone(), false));
                }
                return Step::Next;
            };
            let path = parent.join(name);
            let created = event
                .mask
                .intersects(EventMask::CREATE | EventMask::MOVED_TO);
            let tree = self.depth == Depth::Tree && event.mask.contains(EventMask::ISDIR);
            if tree && event.mask.contains(EventMask::MOVED_FROM) {
                self.tree.forget(&path);
            }
            // An entry's attributes are not a change of it; a watched directory reports its own.
            if !event.mask.contains(EventMask::ATTRIB) && !is_temporary(&path) {
                batch.push(event_now(path.clone(), created));
            }
            if tree && created {
                // Watched, then read: an entry written into it before the watch was in place is
                // reported with it, and one written after is reported as it lands.
                self.tree.add(path, batch);
            }
            Step::Next
        }

        fn rescan(&mut self, batch: &mut Vec<Event>) {
            if self.depth == Depth::Tree {
                // A directory created meanwhile may be unwatched.
                let _ = self.tree.add_below(&self.root, None);
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
                Reported::Released => RELEASES,
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
                tree.add_below(&root, None).map_err(unsubscribe)?;
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
    use notify::event::{AccessKind, AccessMode, ModifyKind};
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
        #[cfg(target_os = "macos")]
        let started = Started::of(&root);
        let watched_root = root.clone();
        let skipped = move |path: &Path| {
            path.ancestors()
                .skip(1)
                .take_while(|ancestor| *ancestor != watched_root)
                .any(&skip)
        };
        let callback_root = root.clone();
        let handler = move |event: notify::Result<notify::Event>| {
            let root = &callback_root;
            let event = match event {
                Ok(event) if !event.need_rescan() => event,
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
                // A writer closing a file it held open for writing: what a file it stood behind is
                // worth ends there, though the name and bytes stay.
                EventKind::Access(AccessKind::Close(AccessMode::Write))
                    if reported == Reported::Released =>
                {
                    false
                }
                EventKind::Access(_) => return,
                // A move in or out of the tree is a change of the watched entries even where only
                // entries are reported.
                EventKind::Modify(ModifyKind::Name(_)) | EventKind::Create(_) => true,
                EventKind::Modify(_) if reported == Reported::Entries => return,
                _ => false,
            };
            let metadata = matches!(event.kind, EventKind::Modify(ModifyKind::Metadata(_)));
            let batch: Vec<Event> = event
                .paths
                .into_iter()
                // A file's attributes are not a change of it; the watched directory reports its own.
                .filter(|path| !metadata || path == root)
                .filter(|path| !is_temporary(path) && !skipped(path))
                .map(|path| event_now(path, created))
                .collect();
            if !batch.is_empty() {
                on_events(&batch);
            }
        };
        // Links are not followed, as on Linux: a linked folder is not part of the tree.
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

    /// The watched directory as it was when its watch started. `FSEvents` numbers an event when its
    /// daemon gets to it, not when the change happened, so a newer number does not mean a newer change.
    #[cfg(target_os = "macos")]
    struct Started {
        dir: std::path::PathBuf,
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

        /// Whether `event` reports only what the directory went through before its watch started.
        fn explains(&self, event: &notify::Event) -> bool {
            !event.paths.is_empty()
                && event
                    .paths
                    .iter()
                    .all(|path| *path == self.dir || *path == self.canonical)
                && Identity::of(&self.dir).as_ref() == Some(&self.identity)
        }
    }

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

/// Filesystems whose entries can change without passing through this kernel, by `statfs` magic: FUSE,
/// NFS, SMB, CIFS, SMB2, 9P, Ceph, AFS.
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

#[cfg(target_os = "linux")]
const FUSE_SUPER_MAGIC: u32 = 0x6573_5546;

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

/// Whether the FUSE filesystem holding `dir` says it tells the kernel of every change, by answering
/// [`crate::native::CHANGE_SIGNAL_XATTR`].
#[cfg(target_os = "linux")]
fn signals_every_change(dir: &Path) -> bool {
    let mut value = [0_u8; 8];
    rustix::fs::getxattr(dir, crate::native::CHANGE_SIGNAL_XATTR, &mut value[..])
        .is_ok_and(|len| value.get(..len) == Some(b"1".as_slice()))
}

/// Elsewhere the filesystem's type is not told apart, so a watch is trusted for local edits only.
#[cfg(not(target_os = "linux"))]
pub(crate) fn sees_every_change(_dir: &Path) -> bool {
    false
}

/// [`Dir::watch`](crate::Dir::watch) and [`crate::native::watch_entries`]: the watch of `path` below
/// `root`, its changes queued as they arrive.
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

/// `path` below `root` as a [`crate::VPath`]; `None` outside it or for a name that is not UTF-8.
fn relative(root: &Path, path: &Path) -> Option<crate::VPath> {
    let parts: Option<Vec<&str>> = path
        .strip_prefix(root)
        .ok()?
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect();
    crate::VPath::new(&parts?.join("/")).ok()
}
