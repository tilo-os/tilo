//! Code a program fetches the first time it is used rather than before its
//! first frame (the browser desktop's lazily loaded modules).
//!
//! The toolkit asks [`Deferred::get`] for the code where it needs it. Until
//! the program installed it, that returns `None` and, once, calls the loader
//! the program registered ([`Deferred::defer`]); the loader fetches the code
//! and hands it over ([`Deferred::install`]) or reports that it could not
//! ([`Deferred::load_failed`]), after which the next use fetches again.
//! Code that can wait for it, such as a request already running
//! asynchronously, awaits [`Deferred::wait`] instead.

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::task::{Poll, Waker};

use tilo_lock::MutexRecover as _;

/// One piece of code a program fetches on first use; see the module
/// documentation.
pub struct Deferred<T> {
    value: OnceLock<T>,
    load: OnceLock<fn()>,
    requested: AtomicBool,
    generation: AtomicU64,
    /// Bumped by every failed fetch, so a waiter learns its fetch failed.
    failures: AtomicU64,
    /// Tasks waiting in [`Self::wait`].
    waiters: Mutex<Vec<Waker>>,
}

impl<T: Copy> Default for Deferred<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy> Deferred<T> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            value: OnceLock::new(),
            load: OnceLock::new(),
            requested: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            failures: AtomicU64::new(0),
            waiters: Mutex::new(Vec::new()),
        }
    }

    /// The code once it is in, fetching it if no fetch is running. `None`
    /// when this fetch fails or the program registered no loader.
    pub fn wait(&self) -> impl Future<Output = Option<T>> + Send + '_
    where
        T: Send + Sync,
    {
        let failures = self.failures.load(Ordering::Acquire);
        std::future::poll_fn(move |cx| {
            if let Some(value) = self.get() {
                return Poll::Ready(Some(value));
            }
            if self.load.get().is_none() || self.failures.load(Ordering::Acquire) != failures {
                return Poll::Ready(None);
            }
            self.waiters.lock_recover().push(cx.waker().clone());
            // The code may have arrived, or the fetch failed, between the
            // checks above and the registration.
            if self.value.get().is_some() || self.failures.load(Ordering::Acquire) != failures {
                cx.waker().wake_by_ref();
            }
            Poll::Pending
        })
    }

    fn wake_waiters(&self) {
        let waiters = std::mem::take(&mut *self.waiters.lock_recover());
        for waiter in waiters {
            waiter.wake();
        }
    }

    /// The code, or `None` while it is still to come (and then fetched).
    #[must_use]
    pub fn get(&self) -> Option<T> {
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
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// How the program fetches the code: `load` starts the fetch and, once
    /// the code is in, calls [`Self::install`] (or [`Self::load_failed`]).
    pub fn defer(&self, load: fn()) {
        let _ = self.load.set(load);
    }

    /// The code is in.
    pub fn install(&self, value: T) {
        if self.value.set(value).is_ok() {
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
        self.wake_waiters();
    }

    /// The fetch failed: the next use fetches again.
    pub fn load_failed(&self) {
        self.requested.store(false, Ordering::Release);
        self.failures.fetch_add(1, Ordering::AcqRel);
        self.wake_waiters();
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

    static WAITED: Deferred<u32> = Deferred::new();

    fn load_waited() {}

    #[test]
    fn a_waiter_gets_the_code_once_it_arrives_and_none_when_its_fetch_fails() {
        use std::future::Future as _;
        use std::pin::pin;
        use std::task::{Context, Poll, Waker};

        let mut cx = Context::from_waker(Waker::noop());
        assert_eq!(
            pin!(WAITED.wait()).poll(&mut cx),
            Poll::Ready(None),
            "no loader"
        );
        WAITED.defer(load_waited);
        let mut failing = pin!(WAITED.wait());
        assert!(failing.as_mut().poll(&mut cx).is_pending());
        WAITED.load_failed();
        assert_eq!(failing.as_mut().poll(&mut cx), Poll::Ready(None));
        let mut arriving = pin!(WAITED.wait());
        assert!(arriving.as_mut().poll(&mut cx).is_pending());
        WAITED.install(9);
        assert_eq!(arriving.as_mut().poll(&mut cx), Poll::Ready(Some(9)));
    }
}
