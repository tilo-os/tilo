//! The app profile: one current-thread runtime on its own thread.

use std::future::Future;
use std::sync::OnceLock;
use std::thread::ThreadId;

use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::{Error, pool};

/// Name of the thread that drives the app runtime.
pub(crate) const APP_THREAD: &str = "tilo-rt";

/// The process-wide app runtime: a current-thread tokio runtime driven by a
/// dedicated thread.
///
/// One thread is enough for an app: its async work is network and IPC
/// waiting, not computation. CPU-bound work goes to [`crate::cpu()`] and
/// blocking calls to [`AppRuntime::spawn_blocking`], so neither stalls the
/// other tasks.
///
/// The runtime starts on first use and lives as long as the process.
#[derive(Debug)]
pub struct AppRuntime {
    handle: Handle,
    thread: ThreadId,
}

static APP: OnceLock<Result<AppRuntime, String>> = OnceLock::new();

/// The app runtime, started on first use.
///
/// # Panics
///
/// When the OS refuses to start the runtime thread. [`try_app`] reports
/// that instead.
#[allow(
    clippy::panic,
    reason = "a process that cannot start one thread cannot run an app; try_app reports it"
)]
pub fn app() -> &'static AppRuntime {
    match try_app() {
        Ok(runtime) => runtime,
        Err(error) => panic!("{error}"),
    }
}

/// The app runtime, started on first use.
///
/// # Errors
///
/// [`Error::Start`] when the runtime thread could not be started. The
/// failure is remembered; later calls report it again without retrying.
pub fn try_app() -> Result<&'static AppRuntime, Error> {
    APP.get_or_init(|| AppRuntime::start().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|message| Error::Start(std::io::Error::other(message.clone())))
}

impl AppRuntime {
    fn start() -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .thread_name("tilo-rt-blocking")
            .max_blocking_threads(pool::APP_BLOCKING_THREADS)
            .build()?;
        let handle = runtime.handle().clone();
        let driver = crate::spawn_thread(APP_THREAD, move || {
            // Drives every task spawned through the handle, for the life of
            // the process.
            let _driving = crate::Driving::enter();
            runtime.block_on(std::future::pending::<()>());
        })?;
        Ok(Self {
            handle,
            thread: driver.thread().id(),
        })
    }

    /// The runtime's handle, for APIs that take one or need a runtime
    /// context (`handle().enter()`) to construct I/O resources.
    pub const fn handle(&self) -> &Handle {
        &self.handle
    }

    /// The thread that drives this runtime.
    pub const fn thread_id(&self) -> ThreadId {
        self.thread
    }

    /// Whether the calling thread is the one driving this runtime.
    pub fn is_runtime_thread(&self) -> bool {
        std::thread::current().id() == self.thread
    }

    /// Run `future` on the runtime thread. Returns immediately; the
    /// caller never polls it.
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.handle.spawn(future)
    }

    /// Run `future` on the runtime thread, then hand its output to
    /// `deliver` there. `deliver` should only publish — complete a
    /// `LatestRequest`, send on a channel — so the runtime thread is free
    /// for the next task. This is the UI handoff: the UI thread spawns, and
    /// `deliver` wakes the event loop with the result.
    pub fn spawn_then<F, D>(&self, future: F, deliver: D) -> JoinHandle<()>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
        D: FnOnce(F::Output) + Send + 'static,
    {
        self.handle.spawn(async move { deliver(future.await) })
    }

    /// Run the future `make` builds, where the future may hold values that
    /// stay on one thread (a browser page's storage handles, where that is
    /// the target): natively it is built and driven on one thread of the
    /// blocking pool, with the runtime's drivers.
    pub fn spawn_pinned<M, F>(&self, make: M) -> JoinHandle<F::Output>
    where
        M: FnOnce() -> F + Send + 'static,
        F: Future + 'static,
        F::Output: Send + 'static,
    {
        let handle = self.handle.clone();
        self.handle.spawn_blocking(move || {
            let _driving = crate::Driving::enter();
            handle.block_on(make())
        })
    }

    /// Run a blocking call on the runtime's bounded blocking pool.
    pub fn spawn_blocking<F, R>(&self, call: F) -> JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        self.handle.spawn_blocking(call)
    }

    /// Wait on `future` from a synchronous thread.
    ///
    /// For threads that own a blocking API (a git transport callback, a
    /// decoder loop, a synchronous library entry point) and need one async
    /// result. The caller polls `future` itself — so it may borrow — while
    /// the runtime thread drives its sockets and timers and runs anything it
    /// spawns. No runtime is built per call.
    ///
    /// # Errors
    ///
    /// [`Error::UiThread`] on a UI thread, [`Error::InsideRuntime`] from a
    /// runtime context, where waiting would stall the tasks that thread is
    /// meant to run.
    pub fn block_on<F: Future>(&self, future: F) -> Result<F::Output, Error> {
        crate::check_may_block()?;
        let _driving = crate::Driving::enter();
        Ok(self.handle.block_on(future))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    use std::time::Duration;

    #[test]
    fn the_app_runtime_runs_on_its_own_thread_and_drives_spawned_tasks() {
        // phase: app_runtime_runs_on_its_own_named_thread
        {
            let runtime = app();
            let caller = std::thread::current().id();
            let (tx, rx) = mpsc::channel();
            runtime.spawn(async move {
                let current = std::thread::current();
                tx.send((current.id(), current.name().map(str::to_owned)))
                    .unwrap();
            });
            let (ran_on, name) = tilo_test_env::recv("the task's thread and name", &rx);
            assert_ne!(ran_on, caller);
            assert_eq!(ran_on, runtime.thread_id());
            assert_eq!(name.as_deref(), Some(APP_THREAD));
            assert!(!runtime.is_runtime_thread());
        }
        // phase: spawning_does_not_enter_a_runtime_on_the_caller
        {
            let runtime = app();
            let _task = runtime.spawn(async {});
            assert!(!crate::in_runtime());
        }
        // phase: app_runtime_drives_timers_and_io_for_spawned_tasks
        {
            let (tx, rx) = mpsc::channel();
            app().spawn_then(
                async {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    "slept"
                },
                move |out| tx.send(out).unwrap(),
            );
            assert_eq!(tilo_test_env::recv("the slept task's output", &rx), "slept");
        }
        // phase: a_pinned_task_may_hold_a_value_that_stays_on_its_thread
        {
            let (tx, rx) = mpsc::channel();
            app().spawn_pinned(move || async move {
                let pinned = std::rc::Rc::new(4);
                tokio::time::sleep(Duration::from_millis(2)).await;
                tx.send(*pinned + 1).unwrap();
            });
            assert_eq!(tilo_test_env::recv("the pinned task's value", &rx), 5);
        }
    }

    #[test]
    fn blocking_is_allowed_off_the_runtime_thread_only() {
        // phase: worker_threads_can_wait_but_the_runtime_thread_cannot
        {
            let runtime = app();
            assert_eq!(runtime.block_on(async { 3 }).unwrap(), 3);
            let nested = runtime
                .block_on(async { app().block_on(async {}) })
                .unwrap();
            assert!(matches!(nested, Err(Error::InsideRuntime)));
        }
        // phase: a_blocked_caller_can_borrow_and_still_use_the_drivers
        {
            let label = String::from("borrowed");
            let seen = app()
                .block_on(async {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                    label.len()
                })
                .unwrap();
            assert_eq!(seen, label.len());
        }
        // phase: blocking_calls_leave_the_runtime_thread_free
        {
            let runtime = app();
            let (release, gate) = mpsc::channel::<()>();
            let blocked = runtime.spawn_blocking(move || gate.recv().unwrap());
            // With the blocking call parked, the runtime thread still serves
            // other tasks.
            assert_eq!(runtime.block_on(async { 5 }).unwrap(), 5);
            release.send(()).unwrap();
            runtime.block_on(blocked).unwrap().unwrap();
        }
    }
}
