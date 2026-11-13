//! A value read from files, kept current by a worker, for a thread that must never wait
//! on the disk: the compositor's main loop. The reader only ever takes the latest
//! value, an [`Arc`], and never opens, stats or lists a path to find out whether it is
//! stale: a stamp checked on every read is itself a lookup, and on a busy disk one
//! lookup can hold the reader for seconds. A directory that does not exist yet is
//! watched through its nearest existing ancestor, so a value whose files appear later
//! (an identity made at first login) follows them. A writer that must see its own write
//! at once asks for [`Watched::refresh`], which reads on the writer's thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, mpsc};

use super::change_watch::{ChangeWatch, watch_changes, watch_presence};

/// Cloning shares the value and its worker.
pub struct Watched<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Clone for Watched<T> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

type Listener = Box<dyn Fn() + Send + Sync>;

struct Shared<T> {
    read: Box<dyn Fn() -> T + Send + Sync>,
    value: RwLock<(u64, Arc<T>)>,
    /// Handed to each read as it starts, so a read that began later is never replaced by an earlier
    /// one.
    tickets: AtomicU64,
    listeners: Mutex<Vec<Listener>>,
    /// Wakes the worker when the last handle goes, so it ends and its watches with it.
    wake: Mutex<mpsc::Sender<()>>,
}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        let _ = self
            .wake
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(());
    }
}

impl<T> Shared<T> {
    /// Whether the value was kept: a read that began later already stored one.
    fn reread(&self) -> bool {
        let ticket = self.tickets.fetch_add(1, Ordering::AcqRel);
        let fresh = Arc::new((self.read)());
        let mut value = self.value.write().unwrap_or_else(PoisonError::into_inner);
        if value.0 > ticket {
            return false;
        }
        *value = (ticket, fresh);
        true
    }

    fn tell_listeners(&self) {
        for listener in self
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            listener();
        }
    }
}

impl<T: Send + Sync + 'static> Watched<T> {
    /// Watches `directories`, reads once on this thread, and from then on reads again on a worker named
    /// `name` after any change in them. The watches are armed before the first read, so a write landing
    /// during it is read again.
    pub fn start(
        name: &str,
        directories: Vec<PathBuf>,
        read: impl Fn() -> T + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let first = read();
        Self::spawn(name, directories, first, true, read)
    }

    /// Reads on a worker named `name`, never on this thread: `unread` stands in until the first read
    /// lands, and [`Self::on_change`] says when it did. The watches are armed before the worker starts.
    pub fn start_unread(
        name: &str,
        directories: Vec<PathBuf>,
        unread: T,
        read: impl Fn() -> T + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        Self::spawn(name, directories, unread, false, read)
    }

    /// Keeps `first`, read on this thread or standing in for a read still to come, and reads again on a
    /// worker named `name` after each change.
    fn spawn(
        name: &str,
        directories: Vec<PathBuf>,
        first: T,
        read_first: bool,
        read: impl Fn() -> T + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let (changed, wakes) = mpsc::channel::<()>();
        let mut watches = Watches::new(directories, changed.clone());
        watches.arm()?;
        let shared = Arc::new(Shared {
            read: Box::new(read),
            value: RwLock::new((0, Arc::new(first))),
            tickets: AtomicU64::new(u64::from(read_first)),
            listeners: Mutex::new(Vec::new()),
            wake: Mutex::new(changed.clone()),
        });
        let worker = Arc::downgrade(&shared);
        std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                // Ends once every handle is gone: the last one wakes it.
                while wakes.recv().is_ok() {
                    // A burst of changes is read once.
                    while wakes.try_recv().is_ok() {}
                    let Some(shared) = worker.upgrade() else {
                        return;
                    };
                    // A directory that appeared is watched itself from now on; one that cannot be is
                    // tried again at the next change.
                    let _ = watches.arm();
                    if shared.reread() {
                        shared.tell_listeners();
                    }
                }
            })?;
        if !read_first {
            // The first read is the worker's: wake it as a change would, so the value arrives the one
            // way values arrive.
            let _ = changed.send(());
        }
        Ok(Self { shared })
    }

    #[must_use]
    pub fn get(&self) -> Arc<T> {
        Arc::clone(
            &self
                .shared
                .value
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .1,
        )
    }

    /// Reads the value again on this thread, now, for a writer in this process that must not see its
    /// own change late.
    pub fn refresh(&self) {
        if self.shared.reread() {
            self.shared.tell_listeners();
        }
    }

    /// Calls `listener` on the worker after each read that changed the value.
    pub fn on_change(&self, listener: impl Fn() + Send + Sync + 'static) {
        self.shared
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Box::new(listener));
    }
}

/// The directories of one value, each watched once it exists and through its nearest existing ancestor
/// until then.
struct Watches {
    directories: Vec<PathBuf>,
    changed: mpsc::Sender<()>,
    /// The directory each target is watched through, and its watch.
    armed: Vec<Option<(PathBuf, ChangeWatch)>>,
}

impl Watches {
    fn new(directories: Vec<PathBuf>, changed: mpsc::Sender<()>) -> Self {
        let armed = directories.iter().map(|_| None).collect();
        Self {
            directories,
            changed,
            armed,
        }
    }

    /// Replaces a watch on an ancestor once a nearer directory exists.
    fn arm(&mut self) -> std::io::Result<()> {
        let mut failed = None;
        for (target, armed) in self.directories.iter().zip(self.armed.iter_mut()) {
            // A directory on the way can appear between finding the nearest one and its watch taking
            // effect, and that watch would never report it, so the search starts again.
            while let Some(through) = nearest_existing(target) {
                if armed.as_ref().is_some_and(|(held, _)| *held == through) {
                    break;
                }
                match watch_through(target, &through, &self.changed) {
                    Ok(watch) => *armed = Some((through, watch)),
                    Err(error) => {
                        failed.get_or_insert(error);
                        break;
                    }
                }
            }
        }
        failed.map_or(Ok(()), Err)
    }
}

/// Sends on `changed` when the target's files change or the entry on the way to it appears, goes or is
/// renamed.
fn watch_through(
    target: &Path,
    through: &Path,
    changed: &mpsc::Sender<()>,
) -> std::io::Result<ChangeWatch> {
    let changed = changed.clone();
    let notify = move || {
        let _ = changed.send(());
    };
    if through == target {
        return watch_changes(through, |_| true, notify);
    }
    // An ancestor reports only the entry on the way to the target, and only whether it exists.
    let watched_target = target.to_path_buf();
    watch_presence(
        through,
        move |path: &Path| watched_target.starts_with(path),
        notify,
    )
}

fn nearest_existing(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|candidate| candidate.is_dir())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    /// At most as long as a loaded test machine takes to report a change.
    fn within(done: impl Fn() -> bool) -> bool {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(10) {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        done()
    }

    fn read_file(path: PathBuf) -> impl Fn() -> String + Send + Sync + 'static {
        move || std::fs::read_to_string(&path).unwrap_or_default()
    }

    #[test]
    fn a_change_in_the_directory_is_read_on_the_worker() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("value");
        std::fs::write(&file, "one").unwrap();
        let watched = Watched::start(
            "watched-test",
            vec![dir.path().to_owned()],
            read_file(file.clone()),
        )
        .unwrap();
        assert_eq!(*watched.get(), "one");
        let told = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&told);
        watched.on_change(move || {
            counter.fetch_add(1, Ordering::AcqRel);
        });
        std::fs::write(dir.path().join("value.tmp"), "two").unwrap();
        std::fs::rename(dir.path().join("value.tmp"), &file).unwrap();
        assert!(
            within(|| *watched.get() == "two"),
            "the change was not read"
        );
        // The value is kept before the listeners are told, on the same worker.
        assert!(
            within(|| told.load(Ordering::Acquire) >= 1),
            "no listener was told"
        );
    }

    #[test]
    fn a_directory_made_later_is_followed() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("not").join("yet");
        let file = dir.join("value");
        let watched =
            Watched::start("watched-test", vec![dir.clone()], read_file(file.clone())).unwrap();
        assert_eq!(*watched.get(), "");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(within(|| {
            let _ = std::fs::write(&file, "made");
            *watched.get() == "made"
        }));
        std::fs::write(&file, "changed").unwrap();
        assert!(within(|| *watched.get() == "changed"));
    }

    #[test]
    fn a_value_started_unread_arrives_from_the_worker() {
        // A read this test holds open until it lets go: starting must not wait for it, and the value
        // given stands in until the read lands.
        let held = std::sync::Arc::new(std::sync::Barrier::new(2));
        let read = std::sync::Arc::clone(&held);
        let watched =
            Watched::start_unread("watched-test", Vec::new(), "unread".to_owned(), move || {
                read.wait();
                "read".to_owned()
            })
            .unwrap();
        assert_eq!(*watched.get(), "unread");
        held.wait();
        assert!(within(|| *watched.get() == "read"), "the read never landed");
    }

    #[test]
    fn a_writer_sees_its_own_write_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("value");
        let watched = Watched::start(
            "watched-test",
            vec![dir.path().to_owned()],
            read_file(file.clone()),
        )
        .unwrap();
        std::fs::write(&file, "mine").unwrap();
        watched.refresh();
        assert_eq!(*watched.get(), "mine");
    }
}
