//! Thread-pool bounds, and the small job pool behind [`crate::io`].

use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};

/// Blocking-pool bound for the app runtime. An app's blocking calls are a
/// handful of D-Bus or file calls at a time; queueing beyond that is fine.
#[cfg(not(target_family = "wasm"))]
pub(crate) const APP_BLOCKING_THREADS: usize = 8;
/// Blocking-pool bound for a daemon runtime.
#[cfg(not(target_family = "wasm"))]
pub(crate) const DAEMON_BLOCKING_THREADS: usize = 32;
/// Blocking-pool bound for the short-lived [`crate::block_on`] runtime.
pub(crate) const BLOCK_ON_BLOCKING_THREADS: usize = 4;

type Job = Box<dyn FnOnce() + Send + 'static>;

/// A fixed set of named threads draining one job queue.
///
/// Threads start with the pool and live as long as the process.
pub(crate) struct JobPool {
    queue: Mutex<Sender<Job>>,
}

impl JobPool {
    /// Start `threads` threads named `{name}-N`. Fails only when not even
    /// one thread could be started.
    pub(crate) fn start(name: &str, threads: usize) -> std::io::Result<Self> {
        let (queue, jobs) = std::sync::mpsc::channel::<Job>();
        let jobs = Arc::new(Mutex::new(jobs));
        let mut started = 0_usize;
        let mut last_error = None;
        for index in 0..threads.max(1) {
            let jobs = Arc::clone(&jobs);
            match crate::spawn_thread(format!("{name}-{index}"), move || drain(&jobs)) {
                Ok(_) => started = started.saturating_add(1),
                Err(error) => last_error = Some(error),
            }
        }
        match (started, last_error) {
            (0, Some(error)) => Err(error),
            _ => Ok(Self {
                queue: Mutex::new(queue),
            }),
        }
    }

    /// Queue `job`. Returns `false` when every pool thread has exited.
    pub(crate) fn submit(&self, job: impl FnOnce() + Send + 'static) -> bool {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(Box::new(job))
            .is_ok()
    }
}

fn drain(jobs: &Mutex<Receiver<Job>>) {
    loop {
        let job = jobs.lock().unwrap_or_else(PoisonError::into_inner).recv();
        let Ok(job) = job else { return };
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
            tracing::warn!("pool job panicked");
        }
    }
}
