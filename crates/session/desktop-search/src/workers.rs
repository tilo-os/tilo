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
use std::sync::mpsc::{Receiver, Sender};
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
}

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
    /// A palette's work, on the host's own background threads.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            background: ui::background::Background::current(),
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
        self.start(job, key);
    }

    /// Run `job` away from the frame, and once it is done let the query that
    /// waited for its source run.
    fn start(&self, job: Job, key: &str) {
        let workers = self.clone();
        let key = key.to_owned();
        let done = key.clone();
        let spawned = self.background.spawn("tilo-search", move || {
            // A source that panics answers nothing rather than taking the
            // worker down with it.
            let hits = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job.query))
                .unwrap_or_default();
            // The search that asked may be gone: nobody reads its channel.
            let _ = job.answer.send(Answer {
                source: job.source,
                hits,
            });
            workers.finished(&done);
        });
        if spawned.is_err() {
            // A host with no threads of its own: the source answers nothing
            // rather than the frame blocking on it.
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.running.remove(&key);
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
            self.start(job, key);
        }
    }
}

/// The answer that arrived by `deadline`: waiting no longer than the frame
/// has left for it.
pub(crate) fn arrived(answers: &Receiver<Answer>, deadline: ui::time::Instant) -> Option<Answer> {
    let now = ui::time::now();
    if now >= deadline {
        return answers.try_recv().ok();
    }
    wait(answers, deadline.saturating_duration_since(now))
}

/// Wait at most `wait` for an answer.
#[cfg(not(all(target_family = "wasm", not(tilo_no_js))))]
fn wait(answers: &Receiver<Answer>, wait: ui::time::Duration) -> Option<Answer> {
    answers
        .recv_timeout(std::time::Duration::from_nanos(
            u64::try_from(wait.as_nanos()).unwrap_or(u64::MAX),
        ))
        .ok()
}

/// A browser page has one thread: it takes what has arrived and the next
/// frame takes the rest, rather than blocking the page on a source.
#[cfg(all(target_family = "wasm", not(tilo_no_js)))]
fn wait(answers: &Receiver<Answer>, _wait: ui::time::Duration) -> Option<Answer> {
    answers.try_recv().ok()
}
