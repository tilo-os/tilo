//! The threads a search asks its sources on: a frame hands each source that
//! can be moved here, and its answers come back to the search that asked,
//! so a source reading a disk index, a mail server or a service never runs
//! on the frame thread and no frame waits for one.
//!
//! One query per source is in flight at a time. A newer query for a source
//! whose answer is still coming replaces any older one still waiting, and
//! waits for the one running; the answer of a search that was replaced
//! while its query ran goes to that search's own channel, which nobody
//! reads any more, rather than into another search's rows.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};

use crate::model::Hit;

/// What a worker sends back: one source's answer to one search.
#[derive(Debug)]
pub(crate) struct Answer {
    /// The source's place in the search that asked it.
    pub(crate) source: usize,
    pub(crate) hits: Vec<Hit>,
}

/// A source's query as a worker runs it: everything it reads is moved with
/// it ([`crate::model::Source::shared`]), so this needs nothing of the
/// frame.
pub(crate) type Query = Box<dyn FnOnce() -> Vec<Hit> + Send + 'static>;

/// One query, as the worker that runs it reads it.
struct Job {
    /// The search that asked: a job of a search since replaced is not
    /// started.
    generation: u64,
    source: usize,
    /// The query, moved off the frame with everything it reads.
    query: Query,
    /// Where the answer goes: the asking search's own channel.
    answer: Sender<Answer>,
    /// What wakes the desk that asked when the answer is in: the desk draws
    /// no frame for a search while a source works, so the answer is what asks
    /// for the frame that shows it.
    wake: ui::wake::WakeHandle,
}

/// What wakes a host's own loop when an answer is in: the loop that asked
/// from off the toolkit's frames (the session's, which owns its search), where
/// no frame's [`ui::wake::WakeHandle`] reaches. `None` where the frame's wake
/// is the host's.
pub type LoopWake = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct State {
    /// The sources whose query is running now.
    running: HashSet<String>,
    /// One query waiting for its source, by source: a newer query replaces
    /// the one still waiting.
    waiting: HashMap<String, Job>,
    /// The search that is showing: a job of an older one is never started.
    generation: u64,
}

/// The threads a palette asks its sources on, kept across the searches of
/// one open palette so a query running for a source is not started again
/// for it while it runs.
#[derive(Clone)]
pub struct SearchWorkers {
    state: Arc<Mutex<State>>,
    /// How the host runs work: a thread pool natively, the page's own queue
    /// in a browser.
    background: ui::background::Background,
    /// What asks the host's own loop for its next pass as a search off the frames
    /// answers, where the frames' wake reaches nothing.
    loop_wake: Option<LoopWake>,
}

impl Default for SearchWorkers {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for SearchWorkers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("SearchWorkers")
            .field("running", &state.running)
            .field("waiting", &state.waiting.len())
            .finish_non_exhaustive()
    }
}

impl SearchWorkers {
    /// A palette's work, on the host's own background threads, its answers waking
    /// the desk's own frames.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            background: ui::background::Background::current(),
            loop_wake: None,
        }
    }

    /// A host's search that no frame answers: its answers wake the host's loop
    /// through `wake` (the session's own, whose search runs on the loop's thread
    /// but never holds it), and never a frame's wake, which reaches nothing there.
    #[must_use]
    pub fn waking(wake: LoopWake) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            background: ui::background::Background::current(),
            loop_wake: Some(wake),
        }
    }

    /// A pool of the same host with its own searches: a host that asks several
    /// finds at once gives each this, so a newer one never retires an older one's
    /// sources — what `begin` does inside one palette, where a newer query does
    /// replace the older.
    #[must_use]
    pub fn fork(&self) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            background: self.background.clone(),
            loop_wake: self.loop_wake.clone(),
        }
    }

    /// A new search replaces the one showing. What still waited for a
    /// source belonged to a search nobody shows, and is not run.
    #[must_use]
    pub fn begin(&self) -> u64 {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;
        state.waiting.retain(|_, job| job.generation >= generation);
        generation
    }

    /// Whether the search `generation` has been replaced by a newer one: a
    /// stream of it that is asked again has nothing left to show.
    #[must_use]
    pub(crate) fn retired(&self, generation: u64) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .generation
            > generation
    }

    /// Ask source `index`, under its `key`, on a worker, unless a query of
    /// that source is already in flight: then this one waits for it.
    ///
    /// Called from the frame that asks its sources, which is the frame of the
    /// desk that shows the answer: its wake is what asks for the frame that
    /// collects and shows it, and it stays with the job while the job waits
    /// for an older query of the same source.
    pub(crate) fn ask(
        &self,
        generation: u64,
        source: usize,
        key: &str,
        query: Query,
        answer: Sender<Answer>,
    ) {
        let job = Job {
            generation,
            source,
            query,
            answer,
            wake: ui::wake::WakeHandle::new(),
        };
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if generation < state.generation {
                // A search this one replaced: nobody reads its answer.
                return;
            }
            if state.running.contains(key) {
                // The source is still answering an older query: the newer
                // one runs as soon as that answer is in.
                state.waiting.insert(key.to_owned(), job);
                return;
            }
            state.running.insert(key.to_owned());
        }
        self.start(job, key, self.loop_wake.as_ref());
    }

    /// Run `job` away from the frame, and once it is done let the query that
    /// waited for its source run. The answer wakes the desk: no frame is
    /// drawn for the search while its sources work. `loop_wake` is the host's
    /// own wake, where it asks the search from off the frames.
    fn start(&self, job: Job, key: &str, loop_wake: Option<&LoopWake>) {
        let workers = self.clone();
        let key = key.to_owned();
        let done = key.clone();
        let Job {
            source,
            query,
            answer,
            wake,
            ..
        } = job;
        let unanswered = answer.clone();
        let asker = wake.clone();
        let asked_loop = loop_wake.cloned();
        let spawned = self.background.spawn("tilo-search", move || {
            // A source that panics answers nothing rather than taking the
            // worker down with it.
            let hits =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(query)).unwrap_or_default();
            // The search that asked may be gone: nobody reads its channel,
            // and the wake finds no search waiting.
            let _ = answer.send(Answer { source, hits });
            wake.wake();
            if let Some(loop_wake) = &asked_loop {
                // The asking host has no frame to wake: its loop is asked for
                // the pass that takes this answer up.
                loop_wake();
            }
            workers.finished(&done);
        });
        if spawned.is_err() {
            // A host with no threads of its own: the source answers nothing
            // rather than the frame blocking on it, and the search ends with
            // the rows the sources that could answer brought, instead of
            // waiting on one that never will.
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.running.remove(&key);
            drop(state);
            let _ = unanswered.send(Answer {
                source,
                hits: Vec::new(),
            });
            asker.wake();
            if let Some(loop_wake) = loop_wake {
                loop_wake();
            }
        }
    }

    /// `key`'s query is done: start the newest query that waited for it,
    /// when the search that asked it still shows.
    fn finished(&self, key: &str) {
        let next = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.running.remove(key);
            // What waited for this source belonged to a search that no
            // longer shows: only the showing search's query is still worth
            // running, whether this one was its own or an older one's.
            let showing = state.generation;
            state.waiting.retain(|_, job| job.generation >= showing);
            let next = state.waiting.remove(key);
            if next.is_some() {
                state.running.insert(key.to_owned());
            }
            next
        };
        if let Some(job) = next {
            self.start(job, key, self.loop_wake.as_ref());
        }
    }
}
