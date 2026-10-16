//! A search index built on a worker thread from generation-stamped sources.
//!
//! The UI tick only polls: it takes in a finished build, and starts a new
//! one when the retained index is stale and none is running. A query never
//! builds, so a large source change cannot turn a keystroke into indexing.

use std::cell::{Ref, RefCell};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tilo_lock::MutexRecover as _;

pub(super) struct BackgroundIndex<G, T> {
    thread_name: &'static str,
    ready: RefCell<Option<(G, T)>>,
    build: RefCell<Option<BuildReply<G, T>>>,
    #[cfg(test)]
    pub(super) probe: std::sync::Arc<BuildProbe>,
}

/// Readiness includes the loop notification: consuming a build must not
/// leave its wake outstanding while the loop parks for an unrelated request.
struct BuildReply<G, T> {
    receiver: Receiver<(G, T)>,
    notification: Arc<Mutex<()>>,
}

impl<G, T> BuildReply<G, T> {
    fn try_recv(&self) -> Result<(G, T), TryRecvError> {
        let _notification = self.notification.lock_recover();
        self.receiver.try_recv()
    }

    fn recv_timeout(&self, bound: Duration) -> Result<(G, T), RecvTimeoutError> {
        // Do not lock while waiting: the publisher needs the lock to send.
        let finished = self.receiver.recv_timeout(bound)?;
        let _notification = self.notification.lock_recover();
        Ok(finished)
    }
}

/// Where and how often builds ran, for tests that keep them off the UI thread.
#[cfg(test)]
#[derive(Default)]
pub(super) struct BuildProbe {
    pub(super) builds: std::sync::atomic::AtomicUsize,
    pub(super) thread: std::sync::Mutex<Option<std::thread::ThreadId>>,
}

impl<G: PartialEq + Send + 'static, T: Send + 'static> BackgroundIndex<G, T> {
    pub(super) fn new(thread_name: &'static str) -> Self {
        Self {
            thread_name,
            ready: RefCell::new(None),
            build: RefCell::new(None),
            #[cfg(test)]
            probe: std::sync::Arc::default(),
        }
    }

    /// Take in a finished build; start `build` when the retained index is not
    /// for the current `generation` and no build runs. `build` returns the generation it
    /// read, captured before its sources, so a source changing meanwhile
    /// leaves the result stale instead of stamping it current.
    pub(super) fn refresh(
        &self,
        generation: impl Fn() -> G,
        build: impl FnOnce() -> (G, T) + Send + 'static,
    ) {
        let receiver = self.build.borrow_mut().take();
        if let Some(receiver) = receiver {
            match receiver.try_recv() {
                Ok(finished) => *self.ready.borrow_mut() = Some(finished),
                Err(TryRecvError::Empty) => *self.build.borrow_mut() = Some(receiver),
                Err(TryRecvError::Disconnected) => {}
            }
        }
        // Reading a finished build may overlap its source's first cache fill.
        // Compare against the current generation, not a pre-poll observation.
        if self.is_ready(&generation()) || self.build.borrow().is_some() {
            return;
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let notification = Arc::new(Mutex::new(()));
        let publish_notification = Arc::clone(&notification);
        #[cfg(test)]
        let probe = std::sync::Arc::clone(&self.probe);
        let wake = crate::shell::search::invocation_wake::waker();
        let spawned = std::thread::Builder::new()
            .name(self.thread_name.into())
            .spawn(move || {
                #[cfg(test)]
                {
                    probe
                        .builds
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    *probe.thread.lock().expect("index build probe lock") =
                        Some(std::thread::current().id());
                }
                let finished = build();
                let _notification = publish_notification.lock_recover();
                if sender.send(finished).is_ok() {
                    wake.wake();
                }
            });
        if spawned.is_ok() {
            *self.build.borrow_mut() = Some(BuildReply {
                receiver,
                notification,
            });
        }
    }

    /// [`Self::refresh`], then wait at most `bound` for the build it starts.
    /// An invocation by address may be a session's first request, with no
    /// earlier search whose build it could take in; refusing it while a
    /// worker still reads the sources would make an address valid only after
    /// some unrelated listing ran first.
    pub(super) fn settle<B>(
        &self,
        bound: Duration,
        generation: impl Fn() -> G,
        build: impl Fn() -> B,
    ) where
        B: FnOnce() -> (G, T) + Send + 'static,
    {
        let started = Instant::now();
        loop {
            self.refresh(&generation, build());
            if self.is_ready(&generation()) {
                return;
            }
            let Some(receiver) = self.build.borrow_mut().take() else {
                return;
            };
            match receiver.recv_timeout(bound.saturating_sub(started.elapsed())) {
                Ok(finished) => *self.ready.borrow_mut() = Some(finished),
                Err(RecvTimeoutError::Timeout) => {
                    *self.build.borrow_mut() = Some(receiver);
                    return;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// The retained index, when it was built from `generation`.
    pub(super) fn current(&self, generation: &G) -> Option<Ref<'_, T>> {
        Ref::filter_map(self.ready.borrow(), |ready| {
            ready
                .as_ref()
                .filter(|(built, _)| built == generation)
                .map(|(_, index)| index)
        })
        .ok()
    }

    pub(super) fn is_ready(&self, generation: &G) -> bool {
        self.ready
            .borrow()
            .as_ref()
            .is_some_and(|(built, _)| built == generation)
    }

    pub(super) fn building(&self) -> bool {
        self.build.borrow().is_some()
    }

    #[cfg(test)]
    pub(super) fn set_ready(&self, generation: G, index: T) {
        *self.ready.borrow_mut() = Some((generation, index));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn a_cold_source_build_is_current_and_notified_before_consumption() {
        let generation = Arc::new(AtomicU64::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let (entered, notification) = mpsc::sync_channel(1);
        let (release, released) = mpsc::sync_channel(1);
        let released = Mutex::new(released);
        let _installed = crate::shell::search::invocation_wake::install(Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            entered.send(()).expect("observe the publication wake");
            released
                .lock()
                .expect("release lock")
                .recv_timeout(Duration::from_secs(10))
                .expect("release the publication wake");
        }));
        let index = BackgroundIndex::new("tilo-test-search-index");
        let source = Arc::clone(&generation);
        index.refresh(
            || generation.load(Ordering::Acquire),
            move || {
                source.store(1, Ordering::Release);
                (1, "cold records")
            },
        );
        notification
            .recv_timeout(Duration::from_secs(10))
            .expect("production worker must notify");
        assert!(
            matches!(
                index
                    .build
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .notification
                    .try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ),
            "the build cannot be consumed while its notification is outstanding"
        );
        release.send(()).expect("finish notification");
        index.settle(
            Duration::from_secs(10),
            || generation.load(Ordering::Acquire),
            || || panic!("a current cold build must not be rebuilt"),
        );
        assert_eq!(
            *index.current(&1).expect("current snapshot"),
            "cold records"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(index.probe.builds.load(Ordering::Relaxed), 1);
    }
}
