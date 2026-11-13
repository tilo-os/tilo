//! How many items each folder in the listing holds, counted off the UI thread.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local};

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct Key {
    path: PathBuf,
    modified: Option<DateTime<Local>>,
    hidden: bool,
}

#[derive(Default)]
struct Shared {
    counts: HashMap<PathBuf, (Key, u64)>,
    queued: HashSet<Key>,
}

pub struct FolderCounts {
    shared: Arc<Mutex<Shared>>,
    generation: Arc<AtomicU64>,
    queue: Mutex<Option<Sender<Key>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    seen: AtomicU64,
}

impl std::fmt::Debug for FolderCounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FolderCounts").finish_non_exhaustive()
    }
}

impl FolderCounts {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            shared: Arc::default(),
            generation: Arc::default(),
            queue: Mutex::new(None),
            wake: Arc::new(wake),
            seen: AtomicU64::new(0),
        }
    }

    pub fn count(
        &self,
        folder: &Path,
        modified: Option<DateTime<Local>>,
        hidden: bool,
    ) -> Option<u64> {
        let key = Key {
            path: folder.to_path_buf(),
            modified,
            hidden,
        };
        let mut shared = self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((known, count)) = shared.counts.get(folder)
            && *known == key
        {
            return Some(*count);
        }
        if shared.queued.insert(key.clone()) {
            drop(shared);
            self.enqueue(key);
        }
        None
    }

    pub fn settle(&self, folders: &[(PathBuf, Option<DateTime<Local>>)], hidden: bool) {
        for (path, modified) in folders {
            if let Some(count) = count_entries(path, hidden) {
                let key = Key {
                    path: path.clone(),
                    modified: *modified,
                    hidden,
                };
                self.shared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .counts
                    .insert(path.clone(), (key, count));
            }
        }
    }

    pub fn take_arrived(&self) -> bool {
        let now = self.generation.load(Ordering::Acquire);
        self.seen.swap(now, Ordering::AcqRel) != now
    }

    fn enqueue(&self, key: Key) {
        let mut queue = self
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(sender) = queue.as_ref()
            && sender.send(key.clone()).is_ok()
        {
            return;
        }
        let (sender, receiver) = channel();
        let worker = Worker {
            shared: Arc::clone(&self.shared),
            generation: Arc::clone(&self.generation),
            wake: Arc::clone(&self.wake),
        };
        let spawned = std::thread::Builder::new()
            .name("files-folder-counts".into())
            .spawn(move || worker.run(&receiver));
        if spawned.is_ok() && sender.send(key).is_ok() {
            *queue = Some(sender);
        }
    }
}

struct Worker {
    shared: Arc<Mutex<Shared>>,
    generation: Arc<AtomicU64>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Worker {
    fn run(&self, receiver: &Receiver<Key>) {
        while let Ok(key) = receiver.recv() {
            let count = count_entries(&key.path, key.hidden);
            let mut shared = self
                .shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            shared.queued.remove(&key);
            if let Some(count) = count {
                shared.counts.insert(key.path.clone(), (key, count));
            }
            drop(shared);
            if count.is_some() {
                self.generation.fetch_add(1, Ordering::AcqRel);
                (self.wake)();
            }
        }
    }
}

fn count_entries(folder: &Path, hidden: bool) -> Option<u64> {
    let entries = std::fs::read_dir(folder).ok()?;
    let mut count = 0_u64;
    for entry in entries.flatten() {
        if hidden || !entry.file_name().to_string_lossy().starts_with('.') {
            count = count.saturating_add(1);
        }
    }
    Some(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn a_folder_is_counted_off_the_ui_thread_and_cached_until_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", ".hidden"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let (woke, wakes) = mpsc::channel();
        let ui_thread = std::thread::current().id();
        let counts = FolderCounts::new(move || {
            woke.send(std::thread::current().id()).unwrap();
        });

        assert_eq!(counts.count(dir.path(), None, false), None);
        let worker = wakes.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        assert_ne!(worker, ui_thread, "counted on the worker's thread");
        assert!(counts.take_arrived());
        assert!(!counts.take_arrived());
        assert_eq!(counts.count(dir.path(), None, false), Some(3));

        assert_eq!(counts.count(dir.path(), None, true), None);
        wakes.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        assert_eq!(counts.count(dir.path(), None, true), Some(4));

        std::fs::write(dir.path().join("c.txt"), b"x").unwrap();
        let later = Some(Local::now());
        assert_eq!(counts.count(dir.path(), later, false), None);
        wakes.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        assert_eq!(counts.count(dir.path(), later, false), Some(4));
    }
}
