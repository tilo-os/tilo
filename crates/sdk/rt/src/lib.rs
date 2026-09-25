//! The one async runtime model for Tilo processes.
//!
//! Tokio is the only async runtime in Tilo. This crate decides how a process
//! runs it, so that no app, daemon or CLI builds runtimes of its own:
//!
//! - **Apps** share one current-thread runtime, [`app()`], on a dedicated
//!   background thread named `tilo-rt`. It is never the UI or render thread.
//!   The UI spawns onto it and gets results back through the event loop's
//!   existing wake mechanism (`tilo_ui`'s `LatestRequest::spawn` and
//!   `LatestTask::spawn` wrap that handoff); nothing on the UI thread waits
//!   on a future.
//! - **Daemons and services** build one multi-thread runtime with
//!   [`daemon()`]: a bounded worker count, a bounded blocking pool, named
//!   threads.
//! - **Synchronous entry points** — a CLI's `main`, a test — use
//!   [`block_on`]. It refuses to run inside a runtime or on the UI thread,
//!   where it would deadlock or freeze a frame.
//! - **CPU-bound bulk work** — hashing, chunking, decoding, indexing — goes to
//!   [`cpu()`], a dedicated pool, never onto an async worker.
//! - **Bulk file I/O** goes through [`io`], std I/O on dedicated threads
//!   behind an interface narrow enough to swap for `io_uring`.
//!
//! Every thread this crate starts is named, so a stack dump or `top -H`
//! attributes it.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

mod app;
mod cpu;
mod daemon;
pub mod io;
mod pool;

use std::cell::Cell;
use std::future::Future;

pub use app::{AppRuntime, app, try_app};
pub use cpu::{CpuTask, cpu, cpu_pool};
pub use daemon::{Daemon, daemon};
pub use tokio::runtime::{Handle, Runtime};
pub use tokio::task::JoinHandle;

/// Why a runtime operation was refused.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The operating system refused a thread or the runtime's drivers.
    #[error("could not start the async runtime: {0}")]
    Start(#[from] std::io::Error),
    /// A synchronous wait was requested from a thread that is already
    /// driving async work; blocking there deadlocks or starves the runtime.
    #[error("cannot block on a future from inside an async runtime; await it instead")]
    InsideRuntime,
    /// A synchronous wait was requested on the UI thread, where it freezes
    /// every frame until the work finishes.
    #[error(
        "cannot block on a future on the UI thread; spawn it and deliver the result to the event loop"
    )]
    UiThread,
    /// The task was cancelled or panicked before producing a value.
    #[error("the task was cancelled or panicked")]
    TaskFailed,
}

thread_local! {
    static UI_THREAD: Cell<bool> = const { Cell::new(false) };
    static DRIVING: Cell<bool> = const { Cell::new(false) };
}

/// Marks the calling thread as driving async tasks until dropped: a runtime
/// worker, the app runtime thread, or a thread inside one of this crate's
/// `block_on`s. Blocking there would stall every task queued behind it.
pub(crate) struct Driving {
    previous: bool,
}

impl Driving {
    pub(crate) fn enter() -> Self {
        Self {
            previous: DRIVING.with(|driving| driving.replace(true)),
        }
    }
}

/// Mark the calling thread as driving async tasks for as long as it lives:
/// a runtime worker.
pub(crate) fn mark_driving() {
    DRIVING.with(|driving| driving.set(true));
}

impl Drop for Driving {
    fn drop(&mut self) {
        let previous = self.previous;
        DRIVING.with(|driving| driving.set(previous));
    }
}

/// Mark the calling thread as a UI (event-loop or render) thread.
///
/// From then on, [`block_on`] and [`AppRuntime::block_on`] refuse to run on
/// it. The toolkit marks its event-loop and render threads; apps need not.
pub fn mark_ui_thread() {
    UI_THREAD.with(|ui| ui.set(true));
}

/// Whether [`mark_ui_thread`] was called on this thread.
pub fn is_ui_thread() -> bool {
    UI_THREAD.with(Cell::get)
}

/// Whether the calling thread is inside a tokio runtime context: a runtime
/// worker, the thread in a `block_on`, a blocking-pool thread, or a thread
/// that entered a handle.
pub fn in_runtime() -> bool {
    Handle::try_current().is_ok()
}

/// Whether the calling thread drives async tasks of a runtime from this
/// crate: a worker, the app runtime thread, or a thread inside one of its
/// `block_on`s. Blocking-pool threads do not count; they exist to block.
pub fn in_async_context() -> bool {
    DRIVING.with(Cell::get)
}

fn check_may_block() -> Result<(), Error> {
    if is_ui_thread() {
        return Err(Error::UiThread);
    }
    if in_async_context() {
        return Err(Error::InsideRuntime);
    }
    Ok(())
}

/// Run `future` to completion on the calling thread.
///
/// For synchronous entry points only: a CLI's `main`, a test, a dedicated
/// worker thread that owns no runtime. The runtime lives for this call; a
/// process that waits on async work repeatedly should keep a runtime (the
/// [`app()`] runtime, or one built with [`daemon()`]) and use its handle.
///
/// # Errors
///
/// [`Error::UiThread`] on a UI thread, [`Error::InsideRuntime`] inside a
/// runtime context, [`Error::Start`] when the runtime cannot be built.
pub fn block_on<F: Future>(future: F) -> Result<F::Output, Error> {
    check_may_block()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .thread_name("tilo-rt-block-on")
        .max_blocking_threads(pool::BLOCK_ON_BLOCKING_THREADS)
        .build()?;
    let _driving = Driving::enter();
    Ok(runtime.block_on(future))
}

/// Start a named OS thread.
///
/// For work that owns its thread for a long time — an event loop, a device
/// poller, a blocking protocol client. Short jobs belong on [`cpu()`], the
/// [`io`] pool, or a runtime's blocking pool instead.
///
/// # Errors
///
/// Returns the OS error when the thread cannot be created.
pub fn spawn_thread<T, F>(
    name: impl Into<String>,
    body: F,
) -> std::io::Result<std::thread::JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new().name(name.into()).spawn(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_on_runs_a_future_that_needs_the_timer_driver() {
        let value = block_on(async {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            7
        })
        .unwrap();
        assert_eq!(value, 7);
    }

    #[test]
    fn block_on_refuses_inside_a_runtime() {
        let nested = block_on(async { block_on(async {}) }).unwrap();
        assert!(matches!(nested, Err(Error::InsideRuntime)));
    }

    #[test]
    fn blocking_pool_threads_may_block() {
        let inner = block_on(async {
            tokio::task::spawn_blocking(|| app().block_on(async { 9 }))
                .await
                .unwrap()
        })
        .unwrap();
        assert_eq!(inner.unwrap(), 9);
    }

    #[test]
    fn block_on_refuses_on_a_ui_thread() {
        let result = spawn_thread("ui-test", || {
            mark_ui_thread();
            block_on(async { 1 })
        })
        .unwrap()
        .join()
        .unwrap();
        assert!(matches!(result, Err(Error::UiThread)));
    }
}
