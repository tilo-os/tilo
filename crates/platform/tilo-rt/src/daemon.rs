//! The daemon profile: one bounded multi-thread runtime per process.

use std::cell::Cell;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use tokio::runtime::Runtime;

use crate::{Error, pool};

/// Builder for a service's runtime. Start with [`daemon()`].
#[derive(Clone, Debug)]
pub struct Daemon {
    name: &'static str,
    workers: usize,
    max_blocking: usize,
}

/// A multi-thread runtime for the service `name`.
///
/// Workers default to the available parallelism, capped at
/// [`Daemon::DEFAULT_MAX_WORKERS`]: a service waits on sockets and disks, and
/// more workers than that only add contention. The blocking pool defaults to
/// [`Daemon::DEFAULT_MAX_BLOCKING`] threads instead of tokio's 512, so a
/// burst of blocking calls queues rather than spawning hundreds of threads.
/// Threads are named `{name}-rt-N`, workers and blocking-pool threads alike:
/// tokio hands a new thread whichever job is queued, so a name given at
/// spawn cannot tell the two apart.
pub fn daemon(name: &'static str) -> Daemon {
    Daemon {
        name,
        workers: std::thread::available_parallelism()
            .map_or(2, std::num::NonZeroUsize::get)
            .clamp(1, Daemon::DEFAULT_MAX_WORKERS),
        max_blocking: Daemon::DEFAULT_MAX_BLOCKING,
    }
}

impl Daemon {
    /// Upper bound on the default worker count.
    pub const DEFAULT_MAX_WORKERS: usize = 4;
    /// Default bound on the blocking pool.
    pub const DEFAULT_MAX_BLOCKING: usize = pool::DAEMON_BLOCKING_THREADS;

    /// Use exactly `workers` async worker threads (at least one).
    #[must_use]
    pub fn workers(mut self, workers: usize) -> Self {
        self.workers = workers.max(1);
        self
    }

    /// Bound the blocking pool at `threads` (at least one).
    #[must_use]
    pub fn max_blocking(mut self, threads: usize) -> Self {
        self.max_blocking = threads.max(1);
        self
    }

    /// Build the runtime.
    ///
    /// # Errors
    ///
    /// The OS error when the runtime's threads or drivers cannot start.
    pub fn build(&self) -> std::io::Result<Runtime> {
        let name = self.name;
        let workers = self.workers;
        let spawned = AtomicUsize::new(0);
        let parked = Arc::new(ParkedWorkers::default());
        let on_park = Arc::clone(&parked);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(workers)
            .max_blocking_threads(self.max_blocking)
            .thread_name_fn(move || {
                format!("{name}-rt-{}", spawned.fetch_add(1, Ordering::Relaxed))
            })
            // Only workers park and unpark; blocking-pool threads never do.
            // That is what marks a worker as driving tasks, so it refuses to
            // block, while blocking-pool threads, which exist to block, may.
            .on_thread_park(move || on_park.worker_parked())
            .on_thread_unpark(crate::mark_driving)
            .build()?;
        // A new worker finds no work and parks at once. Wait for all of them
        // so that none runs a task before it is marked.
        parked.wait_for(workers);
        Ok(runtime)
    }

    /// Build the runtime and run `main` on it from the calling thread —
    /// the replacement for `#[tokio::main]`. The runtime shuts down when
    /// `main` returns.
    ///
    /// # Errors
    ///
    /// [`Error::InsideRuntime`] or [`Error::UiThread`] when the caller may
    /// not block, [`Error::Start`] when the runtime cannot be built.
    pub fn run<F: Future>(&self, main: F) -> Result<F::Output, Error> {
        crate::check_may_block()?;
        let runtime = self.build()?;
        let _driving = crate::Driving::enter();
        Ok(runtime.block_on(main))
    }
}

thread_local! {
    static COUNTED: Cell<bool> = const { Cell::new(false) };
}

/// How many distinct workers have parked since the runtime was built.
#[derive(Default)]
struct ParkedWorkers {
    count: Mutex<usize>,
    changed: Condvar,
}

impl ParkedWorkers {
    fn worker_parked(&self) {
        crate::mark_driving();
        if COUNTED.with(|counted| counted.replace(true)) {
            return;
        }
        let mut count = self.count.lock().unwrap_or_else(PoisonError::into_inner);
        *count = count.saturating_add(1);
        drop(count);
        self.changed.notify_all();
    }

    fn wait_for(&self, workers: usize) {
        let count = self.count.lock().unwrap_or_else(PoisonError::into_inner);
        drop(
            self.changed
                .wait_while(count, |count| *count < workers)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn daemon_threads_are_bounded_and_named() {
        // phase: workers_are_bounded_and_named
        {
            let runtime = daemon("probe").workers(2).build().unwrap();
            assert_eq!(runtime.metrics().num_workers(), 2);
            let names = runtime.block_on(async {
                let mut names = BTreeSet::new();
                for _ in 0..16 {
                    let name =
                        tokio::spawn(async { std::thread::current().name().map(str::to_owned) })
                            .await
                            .unwrap();
                    names.insert(name.unwrap());
                }
                names
            });
            assert!(
                names.iter().all(|name| name.starts_with("probe-rt-")),
                "{names:?}"
            );
        }
        // phase: the_default_worker_count_is_capped
        {
            let runtime = daemon("capped").build().unwrap();
            assert!(runtime.metrics().num_workers() <= Daemon::DEFAULT_MAX_WORKERS);
        }
        // phase: blocking_threads_carry_the_name_but_do_not_drive_tasks
        {
            let runtime = daemon("svc").workers(1).max_blocking(2).build().unwrap();
            let (name, driving) = runtime
                .block_on(async {
                    tokio::task::spawn_blocking(|| {
                        (
                            std::thread::current().name().map(str::to_owned),
                            crate::in_async_context(),
                        )
                    })
                    .await
                })
                .unwrap();
            let name = name.unwrap();
            assert!(name.starts_with("svc-rt-"), "{name}");
            assert!(!driving, "{name} is a blocking-pool thread");
        }
    }

    #[test]
    fn daemon_workers_refuse_to_block_or_nest() {
        // phase: workers_refuse_to_block_but_blocking_threads_may
        {
            let runtime = daemon("guard").workers(1).build().unwrap();
            let (on_worker, on_blocking) = runtime.block_on(async {
                let on_worker = tokio::spawn(async { crate::block_on(async {}) })
                    .await
                    .unwrap();
                let on_blocking = tokio::task::spawn_blocking(|| crate::block_on(async { 1 }))
                    .await
                    .unwrap();
                (on_worker, on_blocking)
            });
            assert!(matches!(on_worker, Err(Error::InsideRuntime)));
            assert_eq!(on_blocking.unwrap(), 1);
        }
        // phase: run_refuses_to_nest
        {
            let outer = daemon("outer")
                .workers(1)
                .run(async { daemon("inner").workers(1).run(async {}) });
            assert!(matches!(outer.unwrap(), Err(Error::InsideRuntime)));
        }
    }
}
