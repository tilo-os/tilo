//! What this process last published to a runtime file, kept in memory.
//!
//! The compositor publishes its display hardware and the person's displays
//! as files so Settings, another process, can read them. Its own readers
//! need not parse those files back: the value is already here, and reading
//! it costs no I/O. [`Published`] holds the last value written, keyed by the
//! file it was written to, so a reader whose runtime directory changed never
//! sees a value meant for another.
//!
//! [`file_reads_on_this_thread`] counts the settings and runtime-file reads
//! this crate makes on the calling thread, so a test can hold a path (a
//! frame, an idle update) to doing none.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use tilo_lock::RwLockRecover as _;

/// One value a process published to `path`, with a generation that moves
/// every time it is published again.
pub struct Published<T> {
    slot: RwLock<Option<(PathBuf, Arc<T>)>>,
    generation: AtomicU64,
}

impl<T> Published<T> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slot: RwLock::new(None),
            generation: AtomicU64::new(0),
        }
    }

    /// Record `value` as published to `path`.
    pub fn set(&self, path: &Path, value: T) {
        *self.slot.write_recover() = Some((path.to_path_buf(), Arc::new(value)));
        self.generation.fetch_add(1, Ordering::Release);
    }

    /// The value last published to `path` by this process, if any.
    #[must_use]
    pub fn get(&self, path: &Path) -> Option<Arc<T>> {
        self.slot
            .read_recover()
            .as_ref()
            .filter(|(published, _)| published == path)
            .map(|(_, value)| Arc::clone(value))
    }

    /// Moves every time a value is published; equal means nothing new.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

impl<T> Default for Published<T> {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    static FILE_READS: Cell<u64> = const { Cell::new(0) };
}

/// Settings-document and runtime-snapshot file reads this crate has made on
/// the calling thread, since the thread started.
#[must_use]
pub fn file_reads_on_this_thread() -> u64 {
    FILE_READS.with(Cell::get)
}

/// Count one file read (or `stat`) on the calling thread.
pub(crate) fn note_file_read() {
    FILE_READS.with(|reads| reads.set(reads.get().saturating_add(1)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_is_returned_only_for_the_file_it_was_published_to() {
        let published = Published::<u32>::new();
        assert_eq!(published.get(Path::new("/run/a")), None);
        let before = published.generation();
        published.set(Path::new("/run/a"), 7);
        assert_eq!(published.get(Path::new("/run/a")).as_deref(), Some(&7));
        assert_eq!(published.get(Path::new("/run/b")), None);
        assert_ne!(published.generation(), before);
    }

    #[test]
    fn reads_are_counted_per_thread() {
        let before = file_reads_on_this_thread();
        note_file_read();
        assert_eq!(file_reads_on_this_thread(), before + 1);
        std::thread::spawn(|| assert_eq!(file_reads_on_this_thread(), 0))
            .join()
            .unwrap();
    }
}
