//! Device-local cloud transfers, isolated by their stable operation IDs.
use crate::file_ops::{Change, FileOp, Outcome, TransferProgress};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use ui::tilo_resource::Settled;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Lifecycle {
    #[default]
    Queued,
    Running,
    Finished,
}

#[derive(Default)]
struct Job {
    cancel: AtomicBool,
    progress: Mutex<TransferProgress>,
    lifecycle: Mutex<Lifecycle>,
}

fn jobs() -> &'static Mutex<HashMap<String, Arc<Job>>> {
    static JOBS: OnceLock<Mutex<HashMap<String, Arc<Job>>>> = OnceLock::new();
    JOBS.get_or_init(Mutex::default)
}

fn job(id: &str) -> Arc<Job> {
    jobs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(id.to_owned())
        .or_default()
        .clone()
}

/// Register before dispatch so cancellation also applies while queued.
pub(crate) fn register(id: &str) {
    let _ = job(id);
}

pub(crate) fn snapshot(id: &str) -> TransferProgress {
    job(id)
        .progress
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

pub(crate) fn cancel(id: &str) {
    job(id).cancel.store(true, Ordering::Release);
}

pub(crate) fn run(op: &FileOp, reverting: Option<&[Change]>) -> Outcome {
    let FileOp::CloudImport { job_id, .. } = op else {
        return Settled::Failed {
            reason: "cloud runner requires a cloud import".into(),
            partial: vec![],
        };
    };
    run_job(job_id, |cancel, progress| {
        crate::file_ops::execute_with_progress(op, reverting, cancel, progress)
    })
}

fn run_job(
    id: &str,
    execute: impl FnOnce(&AtomicBool, &dyn Fn(TransferProgress)) -> Outcome,
) -> Outcome {
    let job = job(id);
    {
        let mut lifecycle = job
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *lifecycle != Lifecycle::Queued {
            return Settled::Failed {
                reason: "this cloud import has already started; retry with a new job ID".into(),
                partial: vec![],
            };
        }
        *lifecycle = Lifecycle::Running;
    }
    let report = |status| {
        *job.progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = status;
    };
    let outcome = if job.cancel.load(Ordering::Acquire) {
        Settled::Failed {
            reason: "cloud import cancelled".into(),
            partial: vec![],
        }
    } else {
        execute(&job.cancel, &report)
    };
    let mut status = snapshot(id);
    status.phase = if job.cancel.load(Ordering::Acquire) {
        "cancelled"
    } else if outcome.is_done() {
        "done"
    } else {
        "failed"
    }
    .into();
    report(status);
    *job.lifecycle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Lifecycle::Finished;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_jobs_isolate_cancellation_and_progress() {
        let first = uuid::Uuid::new_v4().to_string();
        let second = uuid::Uuid::new_v4().to_string();
        let ready = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                run_job(&first, |cancelled, report| {
                    report(TransferProgress {
                        bytes: 11,
                        ..Default::default()
                    });
                    ready.wait();
                    ready.wait();
                    assert!(cancelled.load(Ordering::Acquire));
                    Settled::Failed {
                        reason: "cancelled".into(),
                        partial: vec![],
                    }
                })
            });
            ready.wait();
            let result = run_job(&second, |cancelled, report| {
                report(TransferProgress {
                    bytes: 29,
                    ..Default::default()
                });
                cancel(&first);
                assert!(!cancelled.load(Ordering::Acquire));
                assert_eq!(snapshot(&first).bytes, 11);
                assert_eq!(snapshot(&second).bytes, 29);
                Settled::Done(vec![])
            });
            assert!(result.is_done());
            ready.wait();
            assert!(!worker.join().unwrap().is_done());
        });
        assert_eq!(snapshot(&first).phase, "cancelled");
        assert_eq!(snapshot(&second).phase, "done");
        assert_eq!(snapshot(&first).bytes, 11);
        assert_eq!(snapshot(&second).bytes, 29);
    }

    #[test]
    fn cancellation_before_start_is_not_reset() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().display().to_string();
        let job_id = uuid::Uuid::new_v4().to_string();
        let op = FileOp::CloudImport {
            job_id: job_id.clone(),
            account: "account".into(),
            entries: vec![],
            destination: crate::cloud_transfer::pin_destination(&folder, std::iter::empty())
                .unwrap(),
            folder,
        };
        register(&job_id);
        cancel(&job_id);
        let outcome = run(&op, None);
        assert_eq!(outcome.reason(), Some("cloud import cancelled"));
        assert!(outcome.result().is_empty());
        assert_eq!(snapshot(&job_id).phase, "cancelled");
        assert!(!run(&op, None).is_done());
        assert_eq!(snapshot(&job_id).phase, "cancelled");
    }
}
