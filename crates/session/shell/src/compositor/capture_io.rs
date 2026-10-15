//! Capture work that waits on the disk or a muxer, run off the compositor loop.
//!
//! Finishing a recording waits for the encoder to drain and the muxer to
//! write the file, up to 30 s; saving "recent" first pushes up to 900 frames
//! into a fresh muxer; a screenshot of an 8192² canvas takes seconds to
//! encode, and copying or pinning the last one decodes it again. On the loop
//! thread any of these is a frozen desktop. [`CaptureIo`] runs such work on
//! one `tilo-capture-io` thread, in the order it was submitted, so a command
//! that reads the last artifact (copy, pin, open, send) runs after the write
//! that made it. A job's result comes back to the loop, which takes it on a
//! later frame; the worker wakes the loop when one is ready.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::task::Waker;

/// A result that can stand for a job that panicked, so the loop still hears
/// about every job it submitted.
pub(crate) trait JobResult {
    fn panicked(message: String) -> Self;
}

type Job<T> = Box<dyn FnOnce() -> T + Send>;

struct Finished<T> {
    results: Mutex<VecDeque<T>>,
    arrived: Condvar,
}

impl<T> Finished<T> {
    fn lock(&self) -> MutexGuard<'_, VecDeque<T>> {
        self.results.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn push(&self, result: T) {
        self.lock().push_back(result);
        self.arrived.notify_all();
    }
}

/// One worker thread for capture I/O and the results it handed back.
pub(crate) struct CaptureIo<T> {
    /// The worker's queue; `None` before the first job, and after the worker
    /// could not start (jobs then run on the caller's thread).
    jobs: Option<mpsc::Sender<(Job<T>, Waker)>>,
    started: bool,
    finished: Arc<Finished<T>>,
    /// Submitted jobs whose result has not been taken yet.
    in_flight: usize,
}

impl<T> Default for CaptureIo<T> {
    fn default() -> Self {
        Self {
            jobs: None,
            started: false,
            finished: Arc::new(Finished {
                results: Mutex::new(VecDeque::new()),
                arrived: Condvar::new(),
            }),
            in_flight: 0,
        }
    }
}

impl<T: JobResult + Send + 'static> CaptureIo<T> {
    /// Run `job` after every job submitted before it; returns at once. Its
    /// result is taken by a later [`Self::take_finished`], and the loop that
    /// submitted it is woken when it is ready.
    pub(crate) fn submit(&mut self, job: impl FnOnce() -> T + Send + 'static) {
        self.in_flight = self.in_flight.saturating_add(1);
        let job: Job<T> = Box::new(job);
        // Made on the loop's thread, so it wakes the loop the result is for;
        // a no-op where no loop installed a wake.
        let wake = crate::shell::search::invocation_wake::waker();
        let unsent = match self.worker() {
            Some(jobs) => jobs.send((job, wake)).err().map(|unsent| unsent.0.0),
            None => Some(job),
        };
        // No worker: run it here rather than lose it.
        if let Some(job) = unsent {
            self.jobs = None;
            self.finished.push(run(job));
        }
    }

    fn worker(&mut self) -> Option<&mpsc::Sender<(Job<T>, Waker)>> {
        if !self.started {
            self.started = true;
            let (jobs, received) = mpsc::channel::<(Job<T>, Waker)>();
            let finished = Arc::clone(&self.finished);
            match std::thread::Builder::new()
                .name("tilo-capture-io".into())
                .spawn(move || {
                    for (job, wake) in received {
                        finished.push(run(job));
                        wake.wake();
                    }
                }) {
                Ok(_) => self.jobs = Some(jobs),
                Err(error) => tracing::warn!(%error, "capture I/O worker did not start"),
            }
        }
        self.jobs.as_ref()
    }
}

impl<T> CaptureIo<T> {
    /// The results of finished jobs, in submission order. Never waits.
    pub(crate) fn take_finished(&mut self) -> Vec<T> {
        let taken: Vec<T> = self.finished.lock().drain(..).collect();
        self.in_flight = self.in_flight.saturating_sub(taken.len());
        taken
    }

    /// Whether a result waits to be taken.
    pub(crate) fn has_finished(&self) -> bool {
        !self.finished.lock().is_empty()
    }

    /// Whether a submitted job's result has not been taken yet.
    #[cfg(test)]
    pub(crate) fn is_busy(&self) -> bool {
        self.in_flight > 0
    }

    /// Wait for every submitted job and take the results. Blocks: only for
    /// offline renderers and tests, never the compositor loop.
    pub(crate) fn wait_for_all(&mut self) -> Vec<T> {
        let mut results = self.finished.lock();
        while results.len() < self.in_flight {
            results = self
                .finished
                .arrived
                .wait(results)
                .unwrap_or_else(PoisonError::into_inner);
        }
        let taken: Vec<T> = results.drain(..).collect();
        drop(results);
        self.in_flight = self.in_flight.saturating_sub(taken.len());
        taken
    }
}

fn run<T: JobResult>(job: Job<T>) -> T {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).unwrap_or_else(|panic| {
        let message = panic
            .downcast_ref::<&str>()
            .map(|text| (*text).to_owned())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "capture work panicked".into());
        T::panicked(message)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    impl JobResult for Result<u32, String> {
        fn panicked(message: String) -> Self {
            Err(message)
        }
    }

    /// A job blocked on the disk holds neither the submitter nor a later
    /// look at the results; results come back in submission order, a
    /// panicking job included.
    #[test]
    fn blocked_capture_work_never_holds_the_submitting_thread() {
        let mut io = CaptureIo::<Result<u32, String>>::default();
        let (release, blocked) = mpsc::channel::<()>();
        let ran_on = Arc::new(Mutex::new(None));
        let recorded = Arc::clone(&ran_on);
        io.submit(move || {
            *recorded.lock().unwrap() = Some(std::thread::current().id());
            blocked.recv().map_err(|error| error.to_string())?;
            Ok(1)
        });
        io.submit(|| panic!("a broken muxer"));
        io.submit(|| Ok(3));
        assert!(io.is_busy());
        assert!(
            io.take_finished().is_empty(),
            "nothing finished while blocked"
        );
        assert!(!io.has_finished());

        release.send(()).unwrap();
        let results = io.wait_for_all();
        assert_eq!(
            results,
            vec![Ok(1), Err("a broken muxer".into()), Ok(3)],
            "in submission order, the panic reported"
        );
        assert!(!io.is_busy());
        assert_ne!(
            *ran_on.lock().unwrap(),
            Some(std::thread::current().id()),
            "the job ran off the submitting thread"
        );
    }
}
