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
//! - **Toolkit background work and timers** go through [`background`], the
//!   executor a host installs, and wake the UI through [`wake`].
//! - **A long-lived thread of its own** — an event loop, a device poller —
//!   comes from [`spawn_thread`], never `std::thread::spawn`.
//! - **A pause inside async work** — a retry backoff, a poll interval — is
//!   [`sleep`], never `std::thread::sleep`.
//! - **A helper a process can lose and restart** runs inside
//!   [`panic_boundary::isolate`], so a process whose panic hook aborts for
//!   every other thread lets that one unwind (see [`panic_boundary`]).
//!
//! Every thread this crate starts is named, so a stack dump or `top -H`
//! attributes it.
//!
//! In a browser (`target_family = "wasm"`) no thread is started: [`app()`]
//! tasks run on the page thread from its microtask queue, [`cpu()`] work runs
//! there as an app task, [`io`] refuses, [`spawn_thread`] fails, and nothing
//! may block. [`availability`] answers which [`Feature`]s the running target
//! has, and each feature's `targets` for every target.

#[cfg(not(target_family = "wasm"))]
mod app;
#[cfg(target_family = "wasm")]
#[path = "app_web.rs"]
mod app;
pub mod background;
mod caps;
mod cpu;
#[cfg(not(target_family = "wasm"))]
mod daemon;
pub mod io;
pub mod panic_boundary;
mod pool;
mod timer;
pub use tilo_wake as wake;

use std::cell::Cell;
use std::future::Future;

pub use app::{AppRuntime, app, try_app};
pub use caps::{Availability, Feature, availability, available};
pub use cpu::{CpuTask, cpu, cpu_pool};
#[cfg(not(target_family = "wasm"))]
pub use daemon::{Daemon, daemon};
pub use timer::sleep;
#[cfg(not(target_family = "wasm"))]
pub use timer::timer_now;
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
#[cfg(not(target_family = "wasm"))]
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
    panic_boundary::abort_outside_boundaries_in_release();
    // A browser page's one thread is its UI thread, and it cannot park.
    if cfg!(target_family = "wasm") {
        return Err(Error::UiThread);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .thread_name("tilo-rt-block-on")
        .max_blocking_threads(pool::BLOCK_ON_BLOCKING_THREADS)
        .build()?;
    let _driving = Driving::enter();
    Ok(runtime.block_on(future))
}

/// Run `future` on `runtime` and wait for its output on the calling thread.
///
/// The one way synchronous code calls into a runtime it does not drive — a
/// daemon's network, a service's host — from a thread of its own: the
/// future runs as a task of `runtime`, and the caller waits on a plain
/// channel, outside any runtime. Unlike `Runtime::block_on` on a shared
/// handle, the caller's thread never enters `runtime`, so it may be any
/// thread that may block. Async code awaits the future instead.
///
/// # Errors
///
/// [`Error::UiThread`] on a UI thread, [`Error::InsideRuntime`] on a thread
/// that drives async tasks (a worker of any runtime from this crate,
/// `runtime`'s own included, where waiting would stall the tasks queued
/// behind the caller, or deadlock a runtime with one worker), and
/// [`Error::TaskFailed`] when the task panicked or `runtime` shut down
/// before it finished.
pub fn wait_on<F>(runtime: &Handle, future: F) -> Result<F::Output, Error>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    check_may_block()?;
    if cfg!(target_family = "wasm") {
        return Err(Error::UiThread);
    }
    let (answer, answered) = std::sync::mpsc::sync_channel(1);
    runtime.spawn(async move {
        let _ = answer.send(future.await);
    });
    answered.recv().map_err(|_| Error::TaskFailed)
}

/// Park the calling thread for `duration`: the sanctioned replacement for
/// `std::thread::sleep`. Async code awaits [`sleep`] instead.
///
/// For desktop-only waits — a headless capture pumping a decoder, a device
/// poller on its own thread. A browser page has one thread and cannot park;
/// there this returns at once with an error, where `std::thread::sleep`
/// panics.
///
/// # Errors
///
/// [`Error::UiThread`] on the web.
pub fn sleep_thread(duration: std::time::Duration) -> Result<(), Error> {
    if cfg!(target_family = "wasm") {
        return Err(Error::UiThread);
    }
    std::thread::sleep(duration);
    Ok(())
}

/// Start a named OS thread: the sanctioned replacement for
/// `std::thread::spawn` and `std::thread::Builder::spawn`.
///
/// For work that owns its thread for a long time — an event loop, a device
/// poller, a blocking protocol client. Short jobs belong on [`cpu()`], the
/// [`io`] pool, [`background`], or a runtime's blocking pool instead.
///
/// Unlike `std::thread::spawn`, this never panics: where
/// [`Feature::Threads`] is unsupported (a browser page) it returns an error,
/// and the caller degrades — skips optional work, or runs a short step
/// inline.
///
/// # Errors
///
/// Returns the OS error when the thread cannot be created, and
/// [`std::io::ErrorKind::Unsupported`] on a target without threads.
pub fn spawn_thread<T, F>(
    name: impl Into<String>,
    body: F,
) -> std::io::Result<std::thread::JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    panic_boundary::abort_outside_boundaries_in_release();
    std::thread::Builder::new().name(name.into()).spawn(body)
}

/// How far below an ordinary thread [`spawn_background_thread`] runs, as a
/// Linux nice value.
pub const BACKGROUND_NICE: i32 = 10;

/// [`spawn_thread`] for work a person does not wait on frame by frame —
/// rasterising pages ahead of a scroll, decoding, indexing — that must never
/// take a CPU from the thread drawing the window. On Linux the thread runs
/// [`BACKGROUND_NICE`] steps below its spawner, so on a machine with one or
/// two slow cores the UI thread is scheduled first and the work fills the
/// time it leaves; elsewhere it is an ordinary thread.
///
/// # Errors
///
/// As [`spawn_thread`].
pub fn spawn_background_thread<T, F>(
    name: impl Into<String>,
    body: F,
) -> std::io::Result<std::thread::JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    spawn_thread(name, move || {
        lower_current_thread_priority();
        body()
    })
}

/// Lower the calling thread (only it: Linux keeps a nice value per thread)
/// to [`BACKGROUND_NICE`]. Failure leaves it as it was, which is only slower.
pub fn lower_current_thread_priority() {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `gettid` takes nothing and returns this thread's id.
        let Ok(tid) = libc::id_t::try_from(unsafe { libc::gettid() }) else {
            return;
        };
        // SAFETY: `getpriority`/`setpriority` take and return plain integers;
        // PRIO_PROCESS with a thread id changes that thread alone on Linux.
        unsafe {
            let current = libc::getpriority(libc::PRIO_PROCESS, tid);
            libc::setpriority(
                libc::PRIO_PROCESS,
                tid,
                current.saturating_add(BACKGROUND_NICE).min(19),
            );
        }
    }
}

/// CPU time the calling thread has used, where the platform keeps it: what
/// a frame cost its thread, apart from time spent preempted or waiting.
#[must_use]
pub fn thread_cpu_time() -> Option<std::time::Duration> {
    // macOS has kept the clock since 10.12.
    #[cfg(any(target_os = "linux", target_vendor = "apple"))]
    {
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `now` is a valid, writable timespec for the call.
        let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut now) };
        if status != 0 {
            return None;
        }
        Some(std::time::Duration::new(
            u64::try_from(now.tv_sec).ok()?,
            u32::try_from(now.tv_nsec).ok()?,
        ))
    }
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_background_thread_runs_below_its_spawner() {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: plain integer syscalls on this process's own threads.
            let nice = || unsafe {
                libc::getpriority(
                    libc::PRIO_PROCESS,
                    libc::id_t::try_from(libc::gettid()).unwrap(),
                )
            };
            let spawner = nice();
            let background = spawn_background_thread("nice-test", nice)
                .unwrap()
                .join()
                .unwrap();
            assert_eq!(background, (spawner + BACKGROUND_NICE).min(19));
        }
    }

    /// A thread's CPU clock counts its own work and not time it spends
    /// asleep, where the platform keeps one.
    #[test]
    #[cfg(any(target_os = "linux", target_vendor = "apple"))]
    fn a_threads_cpu_clock_counts_its_work_and_not_its_sleep() {
        let start = thread_cpu_time().expect("the thread's CPU clock");
        std::thread::sleep(std::time::Duration::from_millis(200));
        let slept = thread_cpu_time().unwrap().saturating_sub(start);
        assert!(slept < std::time::Duration::from_millis(100), "{slept:?}");
        let busy = std::time::Instant::now();
        let mut spin = 0_u64;
        while busy.elapsed() < std::time::Duration::from_millis(50) {
            spin = std::hint::black_box(spin.wrapping_add(1));
        }
        assert!(thread_cpu_time().unwrap() > start.saturating_add(slept));
    }

    #[test]
    fn wait_on_runs_on_the_runtime_and_refuses_its_drivers() {
        let runtime = daemon("wait-on-test").workers(1).build().unwrap();
        // phase: a_plain_thread_waits_for_a_task_that_needs_the_timer
        {
            let value = wait_on(runtime.handle(), async {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                tokio::spawn(async { 3 }).await.unwrap()
            })
            .unwrap();
            assert_eq!(value, 3);
        }
        // phase: a_worker_of_the_same_one_worker_runtime_is_refused_not_deadlocked
        {
            let handle = runtime.handle().clone();
            let nested = wait_on(
                runtime.handle(),
                async move { wait_on(&handle, async { 4 }) },
            )
            .unwrap();
            assert!(matches!(nested, Err(Error::InsideRuntime)));
        }
        // phase: a_worker_of_another_runtime_is_refused
        {
            let other = daemon("wait-on-other").workers(1).build().unwrap();
            let handle = runtime.handle().clone();
            let nested =
                wait_on(other.handle(), async move { wait_on(&handle, async { 5 }) }).unwrap();
            assert!(matches!(nested, Err(Error::InsideRuntime)));
        }
        // phase: a_blocking_pool_thread_may_wait
        {
            let handle = runtime.handle().clone();
            let inner = wait_on(runtime.handle(), async move {
                tokio::task::spawn_blocking(move || wait_on(&handle, async { 6 }))
                    .await
                    .unwrap()
            })
            .unwrap();
            assert_eq!(inner.unwrap(), 6);
        }
        // phase: a_ui_thread_is_refused
        {
            let handle = runtime.handle().clone();
            let result = spawn_thread("ui-wait-on-test", move || {
                mark_ui_thread();
                wait_on(&handle, async { 1 })
            })
            .unwrap()
            .join()
            .unwrap();
            assert!(matches!(result, Err(Error::UiThread)));
        }
        // phase: a_shut_down_runtime_answers_task_failed
        {
            let handle = runtime.handle().clone();
            runtime.shutdown_background();
            assert!(matches!(
                wait_on(&handle, async { 2 }),
                Err(Error::TaskFailed)
            ));
        }
    }

    #[test]
    fn block_on_drives_timers_but_refuses_runtime_and_ui_threads() {
        // phase: block_on_runs_a_future_that_needs_the_timer_driver
        {
            let value = block_on(async {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                7
            })
            .unwrap();
            assert_eq!(value, 7);
        }
        // phase: block_on_refuses_inside_a_runtime
        {
            let nested = block_on(async { block_on(async {}) }).unwrap();
            assert!(matches!(nested, Err(Error::InsideRuntime)));
        }
        // phase: blocking_pool_threads_may_block
        {
            let inner = block_on(async {
                tokio::task::spawn_blocking(|| app().block_on(async { 9 }))
                    .await
                    .unwrap()
            })
            .unwrap();
            assert_eq!(inner.unwrap(), 9);
        }
        // phase: block_on_refuses_on_a_ui_thread
        {
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
}
