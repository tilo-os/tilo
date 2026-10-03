//! CPU-bound bulk work, on a dedicated pool.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::task::{Context, Poll};

use tokio::sync::oneshot;

use crate::Error;

/// The process-wide CPU pool: one named thread (`tilo-cpu-N`) per core.
///
/// Use it directly (`cpu_pool().install(|| items.par_iter()…)`) for
/// data-parallel work from a synchronous thread; from async code, use
/// [`cpu()`].
///
/// # Panics
///
/// When the OS refuses to start the pool's threads.
///
/// In a browser the pool is the page thread alone: `install` and parallel
/// iterators run sequentially on the caller.
#[allow(
    clippy::expect_used,
    reason = "a process that cannot start its compute threads cannot do the work it was asked for"
)]
pub fn cpu_pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let builder = rayon::ThreadPoolBuilder::new();
        #[cfg(target_family = "wasm")]
        let builder = builder.num_threads(1).use_current_thread();
        builder
            .thread_name(|index| format!("tilo-cpu-{index}"))
            .build()
            .expect("start the CPU pool")
    })
}

/// Run `work` on the [`cpu_pool`] and await its result.
///
/// Hashing, chunking, compression, image decoding and index building belong
/// here: on an async worker they stall every task behind them, and on a
/// blocking pool they compete with blocking I/O for threads sized for
/// waiting, not computing.
///
/// In a browser the work runs on the page thread as an app task, after the
/// caller yields.
pub fn cpu<R, F>(work: F) -> CpuTask<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let (tx, rx) = oneshot::channel();
    let job = move || {
        if tx.is_closed() {
            return;
        }
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
        let _ = tx.send(outcome.ok());
    };
    #[cfg(not(target_family = "wasm"))]
    cpu_pool().spawn(job);
    #[cfg(target_family = "wasm")]
    crate::app::spawn_job(job);
    CpuTask(rx)
}

/// The pending result of [`cpu()`]. Dropping it discards the result; work
/// that has not started yet is skipped.
#[derive(Debug)]
pub struct CpuTask<R>(oneshot::Receiver<Option<R>>);

impl<R> Future for CpuTask<R> {
    type Output = Result<R, Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0)
            .poll(cx)
            .map(|received| received.ok().flatten().ok_or(Error::TaskFailed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_work_runs_on_the_pool_and_reports_panics() {
        // phase: cpu_work_runs_on_the_cpu_pool_not_the_runtime
        {
            let (name, in_runtime) = crate::block_on(cpu(|| {
                (
                    std::thread::current().name().map(str::to_owned),
                    crate::in_runtime(),
                )
            }))
            .unwrap()
            .unwrap();
            assert!(name.unwrap().starts_with("tilo-cpu-"));
            assert!(!in_runtime);
        }
        // phase: a_panicking_job_reports_failure
        {
            let result = crate::block_on(cpu(|| -> u8 { panic!("job failure") })).unwrap();
            assert!(matches!(result, Err(Error::TaskFailed)));
        }
    }
}
