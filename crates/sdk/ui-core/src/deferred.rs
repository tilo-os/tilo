//! Code a program fetches the first time it is used rather than before its
//! first frame (the browser desktop's lazily loaded modules).
//!
//! The toolkit asks [`Deferred::get`] for the code where it needs it. Until
//! the program installed it, that returns `None` and, once, calls the loader
//! the program registered ([`Deferred::defer`]); the loader fetches the code
//! and hands it over ([`Deferred::install`]) or reports that it could not
//! ([`Deferred::load_failed`]), after which the next use fetches again.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub(crate) struct Deferred<T> {
    value: OnceLock<T>,
    load: OnceLock<fn()>,
    requested: AtomicBool,
    generation: AtomicU64,
}

impl<T: Copy> Deferred<T> {
    pub(crate) const fn new() -> Self {
        Self {
            value: OnceLock::new(),
            load: OnceLock::new(),
            requested: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        }
    }

    /// The code, or `None` while it is still to come (and then fetched).
    pub(crate) fn get(&self) -> Option<T> {
        let value = self.value.get().copied();
        if value.is_none() && !self.requested.swap(true, Ordering::AcqRel) {
            match self.load.get() {
                Some(load) => load(),
                None => self.requested.store(false, Ordering::Release),
            }
        }
        value
    }

    /// Bumped when the code arrives: what was made without it is made again
    /// once this moved.
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// How the program fetches the code: `load` starts the fetch and, once
    /// the code is in, calls [`Self::install`] (or [`Self::load_failed`]).
    pub(crate) fn defer(&self, load: fn()) {
        let _ = self.load.set(load);
    }

    /// The code is in.
    pub(crate) fn install(&self, value: T) {
        if self.value.set(value).is_ok() {
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// The fetch failed: the next use fetches again.
    pub(crate) fn load_failed(&self) {
        self.requested.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::Deferred;

    static LOADS: AtomicUsize = AtomicUsize::new(0);
    static CODE: Deferred<u32> = Deferred::new();

    fn load() {
        LOADS.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn fetches_once_until_it_fails_and_serves_what_arrived() {
        assert_eq!(CODE.get(), None, "nothing to fetch with yet");
        CODE.defer(load);
        assert_eq!(CODE.get(), None);
        assert_eq!(CODE.get(), None);
        assert_eq!(LOADS.load(Ordering::SeqCst), 1, "one fetch at a time");
        CODE.load_failed();
        assert_eq!(CODE.get(), None);
        assert_eq!(LOADS.load(Ordering::SeqCst), 2, "a failed fetch is retried");
        assert_eq!(CODE.generation(), 0);
        CODE.install(7);
        assert_eq!(CODE.get(), Some(7));
        assert_eq!(CODE.generation(), 1);
        assert_eq!(LOADS.load(Ordering::SeqCst), 2);
    }
}
