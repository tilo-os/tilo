//! Background integration for durable, verified folder copies.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use super::migration::{CopyProgress, CopyReport, copy_folder_with_journal, resume_folder};
use super::migration_durability;

const RUNNING: u8 = 0;
const CANCELLATION_REQUESTED: u8 = 1;
const FINISHED: u8 = 2;

/// Whether this platform currently has the native directory-entry durability
/// required by journaled copying. Callers must surface the refusal verbatim.
pub(super) fn durable_copy_available() -> Result<(), String> {
    migration_durability::supported().map_err(|error| error.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JobKind {
    Fresh,
    Resume,
}

/// Exact grants and private journal for one reviewed operation.
#[derive(Clone, Debug)]
pub(super) struct MigrationJob {
    operation_id: String,
    source: PathBuf,
    destination: PathBuf,
    journal: PathBuf,
    kind: JobKind,
}

impl MigrationJob {
    /// Construct a new operation. This validates representation only; the copy
    /// engine pins and revalidates every filesystem grant on the worker thread.
    pub(super) fn fresh(
        operation_id: impl Into<String>,
        source: PathBuf,
        destination: PathBuf,
        journal: PathBuf,
    ) -> Result<Self, String> {
        let operation_id = operation_id.into();
        if operation_id.is_empty()
            || operation_id.len() > 96
            || !operation_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err("migration operation id is invalid".into());
        }
        if !source.is_absolute() || !destination.is_absolute() || !journal.is_absolute() {
            return Err("migration grants and journal must be native absolute paths".into());
        }
        Ok(Self {
            operation_id,
            source,
            destination,
            journal,
            kind: JobKind::Fresh,
        })
    }

    #[must_use]
    pub(super) fn operation_id(&self) -> &str {
        &self.operation_id
    }

    #[must_use]
    pub(super) fn source(&self) -> &Path {
        &self.source
    }

    #[must_use]
    pub(super) fn destination(&self) -> &Path {
        &self.destination
    }

    #[must_use]
    pub(super) fn journal(&self) -> &Path {
        &self.journal
    }

    /// Build an explicit resume request against the existing private journal.
    pub(super) fn retry(&self) -> Result<Self, String> {
        if !self.journal.is_file() {
            return Err(
                "the durable migration journal is unavailable; reselect and review the folders"
                    .into(),
            );
        }
        let mut retry = self.clone();
        retry.kind = JobKind::Resume;
        Ok(retry)
    }
}

#[derive(Debug)]
pub(super) enum WorkerOutcome {
    Complete(CopyReport),
    Incomplete(CopyReport),
    Failed(String),
    Interrupted,
}

#[derive(Debug)]
pub(super) enum WorkerEvent {
    Progress(CopyProgress),
    Finished(WorkerOutcome),
}

/// Owns the only cancellation flag and receives verified engine events.
pub(super) struct MigrationWorker {
    cancelled: Arc<AtomicBool>,
    phase: Arc<AtomicU8>,
    events: Receiver<WorkerEvent>,
    terminal_observed: bool,
}

impl MigrationWorker {
    /// Start one real copy or resume operation on a named background thread.
    pub(super) fn start(job: MigrationJob) -> Result<Self, String> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let phase = Arc::new(AtomicU8::new(RUNNING));
        let (tx, events) = mpsc::channel();
        let worker_cancelled = Arc::clone(&cancelled);
        let worker_phase = Arc::clone(&phase);
        let name = format!("imager-migration-{}", job.operation_id());
        std::thread::Builder::new()
            .name(name)
            .spawn(move || {
                let progress_tx = tx.clone();
                let progress_cancelled = Arc::clone(&worker_cancelled);
                let progress = move |progress| {
                    if progress_tx.send(WorkerEvent::Progress(progress)).is_err() {
                        progress_cancelled.store(true, Ordering::Release);
                    }
                };
                let result = match job.kind {
                    JobKind::Fresh => copy_folder_with_journal(
                        job.source(),
                        job.destination(),
                        job.journal(),
                        &worker_cancelled,
                        progress,
                    ),
                    JobKind::Resume => resume_folder(job.journal(), &worker_cancelled, progress),
                };
                let outcome = match result {
                    Ok(report) if report.complete() => WorkerOutcome::Complete(report),
                    Ok(report) => WorkerOutcome::Incomplete(report),
                    Err(error) => WorkerOutcome::Failed(error),
                };
                worker_phase.store(FINISHED, Ordering::Release);
                let _ = tx.send(WorkerEvent::Finished(outcome));
            })
            .map(drop)
            .map_err(|error| format!("migration worker could not start: {error}"))?;
        Ok(Self {
            cancelled,
            phase,
            events,
            terminal_observed: false,
        })
    }

    /// Request cancellation only while the operation remains active.
    pub(super) fn cancel(&self) -> bool {
        if self
            .phase
            .compare_exchange(
                RUNNING,
                CANCELLATION_REQUESTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return false;
        }
        self.cancelled.store(true, Ordering::Release);
        true
    }

    /// Return the next available update. A lost channel becomes one explicit
    /// interrupted outcome and can never be mistaken for completion.
    pub(super) fn try_next(&mut self) -> Option<WorkerEvent> {
        if self.terminal_observed {
            return None;
        }
        match self.events.try_recv() {
            Ok(event) => {
                if matches!(event, WorkerEvent::Finished(_)) {
                    self.terminal_observed = true;
                    self.phase.store(FINISHED, Ordering::Release);
                }
                Some(event)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.terminal_observed = true;
                self.phase.store(FINISHED, Ordering::Release);
                Some(WorkerEvent::Finished(WorkerOutcome::Interrupted))
            }
        }
    }

    #[cfg(test)]
    fn from_channel_for_test(events: Receiver<WorkerEvent>) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            phase: Arc::new(AtomicU8::new(RUNNING)),
            events,
            terminal_observed: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::*;

    fn fixture() -> (tempfile::TempDir, MigrationJob) {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let source = base.join("source");
        let private = base.join("private");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&private).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        }
        (
            temp,
            MigrationJob::fresh(
                "operation-1",
                source,
                base.join("destination"),
                private.join("operation-1.journal"),
            )
            .unwrap(),
        )
    }

    fn finish(worker: &mut MigrationWorker) -> WorkerOutcome {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(WorkerEvent::Finished(outcome)) = worker.try_next() {
                return outcome;
            }
            assert!(Instant::now() < deadline, "worker did not finish");
            std::thread::yield_now();
        }
    }

    // Catches treating progress or a successful thread exit as completion
    // without consuming the engine's verified terminal report.
    #[test]
    fn completes_only_after_the_verified_report() {
        let (_temp, job) = fixture();
        fs::write(job.source().join("hello"), b"verified bytes").unwrap();
        let mut worker = MigrationWorker::start(job.clone()).unwrap();
        let outcome = finish(&mut worker);

        let WorkerOutcome::Complete(report) = outcome else {
            panic!("expected complete outcome: {outcome:?}");
        };
        assert!(report.complete());
        assert_eq!(report.verified_bytes, 14);
        assert_eq!(
            fs::read(job.destination().join("hello")).unwrap(),
            b"verified bytes"
        );
        assert!(!worker.cancel());
    }

    // Catches a cancel request being reported as success or discarding verified
    // output. The real engine sees the worker-owned cancellation flag.
    #[test]
    fn cancellation_is_an_incomplete_terminal_outcome() {
        let (_temp, job) = fixture();
        fs::write(job.source().join("hello"), vec![7_u8; 1024 * 1024]).unwrap();
        let mut worker = MigrationWorker::start(job).unwrap();
        assert!(worker.cancel());

        let WorkerOutcome::Incomplete(report) = finish(&mut worker) else {
            panic!("cancelled copy was not incomplete");
        };
        assert!(report.cancelled);
        assert!(!report.complete());
    }

    // Catches a lost terminal sender being interpreted as a completed copy.
    #[test]
    fn disconnected_channel_is_interrupted_once() {
        let (tx, rx) = mpsc::channel();
        drop(tx);
        let mut worker = MigrationWorker::from_channel_for_test(rx);

        assert!(matches!(
            worker.try_next(),
            Some(WorkerEvent::Finished(WorkerOutcome::Interrupted))
        ));
        assert!(worker.try_next().is_none());
        assert!(!worker.cancel());
    }

    // Catches retry starting a new overwrite-prone operation instead of asking
    // the journal to revalidate and resume the retained verified output.
    #[test]
    fn explicit_retry_resumes_the_existing_journal() {
        let (_temp, fresh) = fixture();
        for index in 0..32 {
            fs::write(
                fresh.source().join(format!("file-{index}")),
                vec![index as u8; 1024 * 1024],
            )
            .unwrap();
        }
        let mut cancelled = MigrationWorker::start(fresh.clone()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if matches!(cancelled.try_next(), Some(WorkerEvent::Progress(_))) {
                assert!(cancelled.cancel());
                break;
            }
            assert!(Instant::now() < deadline, "worker did not report progress");
            std::thread::yield_now();
        }
        let outcome = finish(&mut cancelled);
        assert!(matches!(outcome, WorkerOutcome::Incomplete(_)));

        let retry = fresh.retry().unwrap();
        let mut resumed = MigrationWorker::start(retry).unwrap();
        let WorkerOutcome::Complete(report) = finish(&mut resumed) else {
            panic!("explicit resume did not complete");
        };
        assert!(report.complete());
        assert_eq!(
            fs::read(fresh.destination().join("file-31")).unwrap(),
            vec![31_u8; 1024 * 1024]
        );
    }
}
