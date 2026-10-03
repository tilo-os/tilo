//! The app profile in a browser page: tasks run on the page's own thread,
//! polled by the browser's microtask queue.
//!
//! A page has one thread and no way to block it, so there is no runtime
//! thread and no tokio driver. Spawned futures join a `LocalSet` that one
//! `wasm_bindgen_futures::spawn_local` task polls for the life of the page;
//! a wake queues a microtask, so a task resumes as soon as the page is idle.
//! Tasks must wait on browser events (promises, fetches, timers from the
//! host) rather than on tokio's `time` or `net`, which have no driver here.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::thread::ThreadId;

use tokio::task::{JoinHandle, LocalSet};

use crate::Error;

/// The process-wide app runtime. In a browser it is the page's microtask
/// queue: every task runs on the page thread, one at a time.
#[derive(Debug)]
pub struct AppRuntime {
    thread: ThreadId,
}

static APP: OnceLock<AppRuntime> = OnceLock::new();

thread_local! {
    static TASKS: &'static LocalSet = start();
}

/// The set lives as long as the page, and one browser task polls it.
fn start() -> &'static LocalSet {
    let tasks: &'static LocalSet = Box::leak(Box::new(LocalSet::new()));
    let mut driving: Pin<Box<dyn Future<Output = ()>>> =
        Box::pin(tasks.run_until(std::future::pending::<()>()));
    wasm_bindgen_futures::spawn_local(std::future::poll_fn(move |cx| {
        let _driving = crate::Driving::enter();
        driving.as_mut().poll(cx)
    }));
    tasks
}

fn spawn_local<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: 'static,
{
    TASKS.with(|tasks| tasks.spawn_local(future))
}

/// The app runtime, started on first use.
pub fn app() -> &'static AppRuntime {
    APP.get_or_init(|| AppRuntime {
        thread: std::thread::current().id(),
    })
}

/// The app runtime. Never fails in a browser: there is no thread to start.
///
/// # Errors
///
/// None; the signature matches native builds.
#[allow(
    clippy::unnecessary_wraps,
    reason = "the same signature as native builds, where starting the runtime thread can fail"
)]
pub fn try_app() -> Result<&'static AppRuntime, Error> {
    Ok(app())
}

impl AppRuntime {
    /// The page thread, which also runs every app task.
    pub const fn thread_id(&self) -> ThreadId {
        self.thread
    }

    /// Whether the caller is inside an app task (the page thread is shared
    /// with the UI, so the thread alone cannot tell).
    pub fn is_runtime_thread(&self) -> bool {
        crate::in_async_context()
    }

    /// Run `future` as an app task. Returns immediately; the task first runs
    /// after the caller yields to the browser.
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        spawn_local(future)
    }

    /// Run `future` as an app task, then hand its output to `deliver`.
    pub fn spawn_then<F, D>(&self, future: F, deliver: D) -> JoinHandle<()>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
        D: FnOnce(F::Output) + Send + 'static,
    {
        spawn_local(async move { deliver(future.await) })
    }

    /// Run the future `make` builds as an app task, where the future may
    /// hold values that stay on one thread (a page's storage handles): here
    /// it is built and polled on the page thread.
    pub fn spawn_pinned<M, F>(&self, make: M) -> JoinHandle<F::Output>
    where
        M: FnOnce() -> F + Send + 'static,
        F: Future + 'static,
        F::Output: Send + 'static,
    {
        spawn_local(async move { make().await })
    }

    /// Run `call` as an app task. A page has no blocking pool: the call runs
    /// on the page thread once the caller yields, so it must be short.
    pub fn spawn_blocking<F, R>(&self, call: F) -> JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        spawn_local(async move { call() })
    }

    /// A page cannot wait on a future synchronously: its one thread is the
    /// UI thread.
    ///
    /// # Errors
    ///
    /// Always [`Error::UiThread`].
    pub fn block_on<F: Future>(&self, _future: F) -> Result<F::Output, Error> {
        Err(Error::UiThread)
    }
}

/// Run `job` as an app task, for [`crate::cpu()`].
pub(crate) fn spawn_job(job: impl FnOnce() + Send + 'static) {
    drop(spawn_local(async move { job() }));
}
