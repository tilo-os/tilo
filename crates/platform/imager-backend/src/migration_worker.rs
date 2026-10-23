//! Background integration for durable, verified folder copies.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

use super::migration::{CopyProgress, CopyReport, copy_folder_with_journal, resume_folder};
use super::migration_durability;
use super::migration_store::{OperationRecord, StoreWriter};

const RUNNING: u8 = 0;
const CANCELLATION_REQUESTED: u8 = 1;
const FINISHED: u8 = 2;

/// Whether this platform currently has the native directory-entry durability
/// required by journaled copying. Callers must surface the refusal verbatim.
pub fn durable_copy_available() -> Result<(), String> {
    migration_durability::supported().map_err(|error| error.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JobKind {
    Fresh,
    Resume,
}

/// Exact grants and private journal for one reviewed operation.
#[derive(Clone, Debug)]
pub struct MigrationJob {
    operation_id: String,
    source: PathBuf,
    destination: PathBuf,
    journal: PathBuf,
    kind: JobKind,
    reviewed: Option<(
        migration_durability::Identity,
        migration_durability::Identity,
        [u8; 32],
    )>,
}

impl MigrationJob {
    /// Construct a new operation. This validates representation only; the copy
    /// engine pins and revalidates every filesystem grant on the worker thread.
    pub fn fresh(
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
            reviewed: None,
        })
    }

    #[must_use]
    pub fn with_reviewed_identities(
        mut self,
        source: migration_durability::Identity,
        parent: migration_durability::Identity,
        revision: [u8; 32],
    ) -> Self {
        self.reviewed = Some((source, parent, revision));
        self
    }

    #[must_use]
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
    }

    #[must_use]
    pub fn destination(&self) -> &Path {
        &self.destination
    }

    #[must_use]
    pub fn journal(&self) -> &Path {
        &self.journal
    }

    /// Build an explicit resume request against the existing private journal.
    pub fn retry(&self) -> Result<Self, String> {
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
pub enum WorkerOutcome {
    Complete(CopyReport),
    Incomplete(CopyReport),
    Failed(String),
    Interrupted,
}

#[derive(Debug)]
pub enum WorkerEvent {
    Progress(CopyProgress),
    Finished(WorkerOutcome),
    Stored(Box<OperationRecord>),
}

/// Owns the only cancellation flag and receives verified engine events.
pub struct MigrationWorker {
    cancelled: Arc<AtomicBool>,
    phase: Arc<AtomicU8>,
    events: Receiver<WorkerEvent>,
    progress: Arc<Mutex<Option<CopyProgress>>>,
    terminal_observed: bool,
}

impl MigrationWorker {
    /// Start one real copy or resume operation on a named background thread.
    pub fn start(job: MigrationJob) -> Result<Self, String> {
        Self::start_inner(job, None)
    }

    pub fn start_persisted(
        job: MigrationJob,
        writer: StoreWriter,
        operations: Vec<OperationRecord>,
        index: usize,
    ) -> Result<Self, String> {
        if index >= operations.len() {
            return Err("copy operation index is unavailable".into());
        }
        Self::start_inner(job, Some((writer, operations, index)))
    }

    fn start_inner(
        job: MigrationJob,
        persistence: Option<(StoreWriter, Vec<OperationRecord>, usize)>,
    ) -> Result<Self, String> {
        let progress_slot = Arc::new(Mutex::new(None));
        let worker_progress = Arc::clone(&progress_slot);
        let cancelled = Arc::new(AtomicBool::new(false));
        let phase = Arc::new(AtomicU8::new(RUNNING));
        let (tx, events) = mpsc::channel();
        let worker_cancelled = Arc::clone(&cancelled);
        let worker_phase = Arc::clone(&phase);
        let name = format!("imager-migration-{}", job.operation_id());
        std::thread::Builder::new()
            .name(name)
            .spawn(move || {
                if let Some((writer, operations, _)) = &persistence
                    && let Err(error) = writer.checkpoint(operations.clone())
                {
                    let _ = tx.send(WorkerEvent::Finished(WorkerOutcome::Failed(format!(
                        "Copy did not start because its state could not be saved: {error}"
                    ))));
                    worker_phase.store(FINISHED, Ordering::Release);
                    return;
                }
                let progress = move |progress| {
                    if let Ok(mut latest) = worker_progress.lock() {
                        *latest = Some(progress);
                    }
                };
                let result = if let Some(reviewed) = &job.reviewed {
                    super::migration_journal::copy_reviewed(
                        job.source(),
                        job.destination(),
                        job.journal(),
                        reviewed,
                        job.kind == JobKind::Resume,
                        &worker_cancelled,
                        progress,
                    )
                } else {
                    match job.kind {
                        JobKind::Fresh => copy_folder_with_journal(
                            job.source(),
                            job.destination(),
                            job.journal(),
                            &worker_cancelled,
                            progress,
                        ),
                        JobKind::Resume => {
                            resume_folder(job.journal(), &worker_cancelled, progress)
                        }
                    }
                };
                let outcome = match result {
                    Ok(report) if report.complete() => WorkerOutcome::Complete(report),
                    Ok(report) => WorkerOutcome::Incomplete(report),
                    Err(error) => WorkerOutcome::Failed(error),
                };
                let event = if let Some((writer, mut operations, index)) = persistence {
                    if let Some(operation) = operations.get_mut(index) {
                        operation.apply_outcome(outcome);
                    }
                    match writer.checkpoint(operations.clone()) {
                        Ok(saved) => operations = saved,
                        Err(error) => {
                            if let Some(operation) = operations.get_mut(index) {
                                operation.persistence_failed(&error);
                            }
                        }
                    }
                    let mut operation = operations.swap_remove(index);
                    operation.compact();
                    WorkerEvent::Stored(Box::new(operation))
                } else {
                    WorkerEvent::Finished(outcome)
                };
                let _ = tx.send(event);
                worker_phase.store(FINISHED, Ordering::Release);
            })
            .map(drop)
            .map_err(|error| format!("migration worker could not start: {error}"))?;
        Ok(Self {
            cancelled,
            phase,
            events,
            progress: progress_slot,
            terminal_observed: false,
        })
    }

    /// Request cancellation only while the operation remains active.
    pub fn cancel(&self) -> bool {
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
    pub fn try_next(&mut self) -> Option<WorkerEvent> {
        if self.terminal_observed {
            return None;
        }
        // Terminal always wins over a continuously updating progress slot.
        match self.events.try_recv() {
            Ok(event) => {
                if matches!(event, WorkerEvent::Finished(_) | WorkerEvent::Stored(_)) {
                    self.terminal_observed = true;
                    self.phase.store(FINISHED, Ordering::Release);
                }
                Some(event)
            }
            Err(TryRecvError::Empty) => {
                self.progress.lock().ok()?.take().map(WorkerEvent::Progress)
            }
            Err(TryRecvError::Disconnected) => {
                self.terminal_observed = true;
                self.phase.store(FINISHED, Ordering::Release);
                Some(WorkerEvent::Finished(WorkerOutcome::Interrupted))
            }
        }
    }

    /// Block until the engine's terminal channel event arrives. Progress lives
    /// in a separate slot and never wakes this wait, so a slow runner cannot
    /// fail a correct result the way a spin-plus-deadline loop can.
    #[cfg(any(test, feature = "test-support"))]
    #[allow(
        clippy::panic,
        reason = "a test helper: a worker that never finishes fails the test"
    )]
    pub fn wait_terminal(&mut self) -> WorkerEvent {
        use std::sync::mpsc::RecvTimeoutError;
        assert!(
            !self.terminal_observed,
            "worker terminal event already observed"
        );
        match self.events.recv_timeout(tilo_test_env::hang_bound()) {
            Ok(event) => {
                assert!(
                    matches!(event, WorkerEvent::Finished(_) | WorkerEvent::Stored(_)),
                    "migration channel carries only terminal events; got {event:?}"
                );
                self.terminal_observed = true;
                self.phase.store(FINISHED, Ordering::Release);
                event
            }
            Err(RecvTimeoutError::Timeout) => panic!("worker did not finish"),
            Err(RecvTimeoutError::Disconnected) => {
                self.terminal_observed = true;
                self.phase.store(FINISHED, Ordering::Release);
                WorkerEvent::Finished(WorkerOutcome::Interrupted)
            }
        }
    }

    #[cfg(test)]
    fn from_channel_for_test(events: Receiver<WorkerEvent>) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            phase: Arc::new(AtomicU8::new(RUNNING)),
            events,
            progress: Arc::new(Mutex::new(None)),
            terminal_observed: false,
        }
    }
}

impl Drop for MigrationWorker {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::mpsc;
    use std::time::Duration;

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
        match worker.wait_terminal() {
            WorkerEvent::Finished(outcome) => outcome,
            event => panic!("expected finished terminal: {event:?}"),
        }
    }

    /// Progress coalescing, completion, cancellation, disconnection and retry.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn migration_worker_terminal_events_are_ordered_and_distinct() {
        // Phase `progress_burst_is_coalesced_before_the_terminal_event`.
        // An unread burst must occupy one latest-value slot, without delaying the
        // terminal result or requiring the UI to consume every per-file update.
        {
            let (_temp, job) = fixture();
            for n in 0..4 {
                fs::write(job.source().join(format!("{n:03}")), b"x").unwrap();
            }
            let mut worker = MigrationWorker::start(job).unwrap();
            let deadline = tilo_test_env::hang_deadline();
            while worker.phase.load(Ordering::Acquire) != FINISHED {
                assert!(
                    std::time::Instant::now() < deadline,
                    "worker did not finish"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            let mut updates = 0;
            loop {
                match worker.try_next() {
                    Some(WorkerEvent::Progress(_)) => updates += 1,
                    Some(WorkerEvent::Finished(WorkerOutcome::Complete(_))) => break,
                    event => panic!("missing terminal event: {event:?}"),
                }
            }
            assert!(
                updates <= 1,
                "UI must drain {updates} stale progress updates"
            );
        }

        // Phase `completes_only_after_the_verified_report`.
        // Catches treating progress or a successful thread exit as completion
        // without consuming the engine's verified terminal report.
        {
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

        // Phase `cancellation_is_an_incomplete_terminal_outcome`.
        // Catches a cancel request being reported as success or discarding verified
        // output. The real engine sees the worker-owned cancellation flag.
        {
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

        {
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

        // Phase `explicit_retry_resumes_the_existing_journal`.
        // Catches retry starting a new overwrite-prone operation instead of asking
        // the journal to revalidate and resume the retained verified output.
        {
            let (_temp, fresh) = fixture();
            for index in 0..32 {
                fs::write(
                    fresh.source().join(format!("file-{index}")),
                    vec![index as u8; 1024 * 1024],
                )
                .unwrap();
            }
            let mut cancelled = MigrationWorker::start(fresh.clone()).unwrap();
            let deadline = tilo_test_env::hang_deadline();
            loop {
                if matches!(cancelled.try_next(), Some(WorkerEvent::Progress(_))) {
                    assert!(cancelled.cancel());
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "worker did not report progress"
                );
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
}
