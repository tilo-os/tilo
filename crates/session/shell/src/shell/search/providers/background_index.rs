//! A search index built on a worker thread from generation-stamped sources.
//!
//! The UI tick only polls: it takes in a finished build, and starts a new
//! one when the retained index is stale and none is running. A query never
//! builds, so a large source change cannot turn a keystroke into indexing.

use std::cell::{Ref, RefCell};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

pub(super) struct BackgroundIndex<G, T> {
    thread_name: &'static str,
    ready: RefCell<Option<(G, T)>>,
    build: RefCell<Option<Receiver<(G, T)>>>,
    #[cfg(test)]
    pub(super) probe: std::sync::Arc<BuildProbe>,
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
    /// for `generation` and no build runs. `build` returns the generation it
    /// read, captured before its sources, so a source changing meanwhile
    /// leaves the result stale instead of stamping it current.
    pub(super) fn refresh(&self, generation: &G, build: impl FnOnce() -> (G, T) + Send + 'static) {
        let receiver = self.build.borrow_mut().take();
        if let Some(receiver) = receiver {
            match receiver.try_recv() {
                Ok(finished) => *self.ready.borrow_mut() = Some(finished),
                Err(TryRecvError::Empty) => *self.build.borrow_mut() = Some(receiver),
                Err(TryRecvError::Disconnected) => {}
            }
        }
        if self.is_ready(generation) || self.build.borrow().is_some() {
            return;
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        #[cfg(test)]
        let probe = std::sync::Arc::clone(&self.probe);
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
                let _ = sender.send(build());
            });
        if spawned.is_ok() {
            *self.build.borrow_mut() = Some(receiver);
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
            let current = generation();
            self.refresh(&current, build());
            if self.is_ready(&current) {
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

    #[cfg(test)]
    pub(super) fn building(&self) -> bool {
        self.build.borrow().is_some()
    }

    #[cfg(test)]
    pub(super) fn set_ready(&self, generation: G, index: T) {
        *self.ready.borrow_mut() = Some((generation, index));
    }
}
