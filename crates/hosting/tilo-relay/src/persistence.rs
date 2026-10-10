//! Ordered, bounded state work. The owner awaits publication before dequeuing
//! its successor; callers only wait on receipts and cannot cancel admitted work.
//!
//! # What a commit guarantees after a crash
//!
//! An admitted delta is fanned out to other sessions, and a `Commit` is
//! answered with `Committed`, only after its owner job returned: its retained
//! record (or pending fallback) and every directory entry leading to it have
//! been fsynced. So an edit any peer has received, or any author was told is
//! committed, survives a crash or power loss in retained history, in order.
//! There is no group commit: no edit waits for another to share a barrier,
//! and none is published before its own barrier. One edit costs two fsyncs
//! (the record, then its directory), plus [`State::next_sequence`]'s
//! reservation renewal at most once per [`SEQUENCE_RESERVATION_NS`] of host
//! clock per workspace.
//!
//! Host order (`seq`, `Committed::seq`) increases strictly across restarts,
//! even when the clock steps back over one, and resumes at most
//! [`SEQUENCE_RESERVATION_NS`] ahead of the clock after a restart.
use crate::store::bounded::Invariant;
use crate::store::replay::{Step, WorkLimit};
use crate::{
    store::{StoreError, WorkspaceStore},
    workspace::SharedWorkspace,
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
#[cfg(test)]
mod durability_tests;
mod replay;
pub(crate) use replay::output_budget;
pub(crate) use replay::{
    Authority as ReplayAuthority, ReplayAdvance, ReplayHandle, ReplayTicket, WrittenBatch,
};

const QUEUE_MESSAGES: usize = 32;
const GLOBAL_MESSAGES: usize = 128;
const GLOBAL_BYTES: usize = 256 * 1024 * 1024;
const REPLAY_MESSAGES: usize = crate::store::replay::DEFAULT_REPLAY_TICKETS;
const REPLAY_BYTES: usize = 64 * 1024;
const NORMAL_MESSAGES: usize = GLOBAL_MESSAGES - REPLAY_MESSAGES;
const NORMAL_BYTES: usize = GLOBAL_BYTES - REPLAY_BYTES;
const BLOCKING_WORKERS: usize = 4;
const RECLAIM_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);
const RECLAIM_RETRY: std::time::Duration = std::time::Duration::from_millis(100);
/// How far past the sequence it hands out [`State::next_sequence`] reserves
/// on disk. A restart continues past the reservation, at most this far
/// ahead of the host clock. Sequences follow the clock, so a workspace that
/// commits steadily renews the reservation once per this interval: at a
/// second, a typing room paid a renewal every second.
const SEQUENCE_RESERVATION_NS: u64 = 10_000_000_000;
/// One blocking-pool task runs a transaction's quanta for up to this long: a
/// many-quantum transaction then pays a pool round trip per slice instead of
/// per quantum, and other workspaces still get a worker within a slice.
const SLICE: std::time::Duration = std::time::Duration::from_millis(4);

type Job = Box<dyn FnOnce(Arc<Mutex<State>>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;
struct NormalJob {
    sequence: u64,
    job: Job,
}
struct ReplayJob {
    prerequisite: Option<u64>,
    job: Job,
}

fn message_budget() -> &'static Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(Semaphore::new(NORMAL_MESSAGES)))
}

fn byte_budget() -> &'static Arc<Semaphore> {
    static BYTES: OnceLock<Arc<Semaphore>> = OnceLock::new();
    BYTES.get_or_init(|| Arc::new(Semaphore::new(NORMAL_BYTES)))
}

fn replay_message_budget() -> &'static Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(Semaphore::new(REPLAY_MESSAGES)))
}

fn replay_byte_budget() -> &'static Arc<Semaphore> {
    static BYTES: OnceLock<Arc<Semaphore>> = OnceLock::new();
    BYTES.get_or_init(|| Arc::new(Semaphore::new(REPLAY_BYTES)))
}

fn detached_replay_budget() -> &'static Arc<Semaphore> {
    static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    WORKERS.get_or_init(|| Arc::new(Semaphore::new(1)))
}

#[derive(Clone)]
struct AdmissionBudgets {
    normal_messages: Arc<Semaphore>,
    normal_bytes: Arc<Semaphore>,
    replay_messages: Arc<Semaphore>,
    replay_bytes: Arc<Semaphore>,
    workers: Arc<Semaphore>,
    replay_worker: Arc<Semaphore>,
}
impl AdmissionBudgets {
    fn global() -> Self {
        Self {
            normal_messages: message_budget().clone(),
            normal_bytes: byte_budget().clone(),
            replay_messages: replay_message_budget().clone(),
            replay_bytes: replay_byte_budget().clone(),
            workers: blocking_budget().clone(),
            replay_worker: detached_replay_budget().clone(),
        }
    }

    #[cfg(test)]
    fn isolated(workers: Arc<Semaphore>) -> Self {
        Self {
            normal_messages: Arc::new(Semaphore::new(NORMAL_MESSAGES)),
            normal_bytes: Arc::new(Semaphore::new(NORMAL_BYTES)),
            replay_messages: Arc::new(Semaphore::new(REPLAY_MESSAGES)),
            replay_bytes: Arc::new(Semaphore::new(REPLAY_BYTES)),
            workers,
            replay_worker: Arc::new(Semaphore::new(1)),
        }
    }
}

fn charged_bytes(bytes: usize) -> Result<u32, StoreError> {
    let bytes = bytes.max(1);
    if bytes > NORMAL_BYTES {
        return Err(error("state byte backpressure"));
    }
    u32::try_from(bytes).map_err(|_| error("state byte backpressure"))
}

fn next_normal_job(sequence: &AtomicU64) -> Result<u64, StoreError> {
    sequence
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1)
        })
        .map(|previous| previous.saturating_add(1))
        .map_err(|_| error("state job sequence exhausted"))
}

#[derive(Clone)]
pub(crate) struct Persistence {
    tx: mpsc::Sender<NormalJob>,
    replay_tx: mpsc::Sender<ReplayJob>,
    normal_sequence: Arc<AtomicU64>,
    admission: Arc<tokio::sync::Mutex<()>>,
    budgets: AdmissionBudgets,
    #[cfg(test)]
    admission_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(test)]
    replay_admission_hook: Option<Arc<dyn Fn() + Send + Sync>>,
}
pub(crate) struct State {
    pub(crate) store: WorkspaceStore,
    pub(crate) doc: Option<automerge::AutoCommit>,
    sequence: Option<u64>,
    /// The bound `state-sequence` holds; 0 until this owner wrote one.
    reserved: u64,
    replays: replay::Replays,
    pub(crate) retention_scratch: Option<std::path::PathBuf>,
    pub(crate) retention_candidates: Vec<(std::path::PathBuf, std::path::PathBuf)>,
    workers: Arc<Semaphore>,
    replay_worker: Arc<Semaphore>,
}
pub(crate) fn error(message: impl Into<String>) -> StoreError {
    StoreError::Io(std::io::Error::other(message.into()))
}

impl State {
    pub(crate) fn next_sequence(&mut self) -> Result<u64, StoreError> {
        self.next_sequence_at(tilo_clock::unix_now_ns())
    }
    /// [`Self::next_sequence`] with the host clock reading `now`.
    fn next_sequence_at(&mut self, now: u64) -> Result<u64, StoreError> {
        let path = self.store.root().join("state-sequence");
        let previous = match self.sequence {
            Some(previous) => previous,
            None => match std::fs::read(&path) {
                Ok(bytes) => u64::from_le_bytes(
                    bytes
                        .try_into()
                        .map_err(|_| error("invalid state sequence"))?,
                ),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
                Err(e) => return Err(e.into()),
            },
        };
        let next = previous
            .checked_add(1)
            .ok_or_else(|| error("state sequence exhausted"))?
            .max(now);
        // Reserve before any data publication, including across restart. The
        // file holds a bound past every sequence handed out, so a burst of
        // commits syncs it once per reservation instead of once per commit.
        if next > self.reserved {
            let reserved = next.saturating_add(SEQUENCE_RESERVATION_NS);
            // Syncs the file, and after the rename the store root holding it.
            crate::store::atomic_write(&path, &reserved.to_le_bytes())?;
            // Apple's fsync leaves the drive's cache unflushed; std's
            // `sync_all` there is F_FULLFSYNC (as `retained::durable_write`).
            #[cfg(target_vendor = "apple")]
            crate::store::fsync(&std::fs::File::open(self.store.root())?)?;
            self.reserved = reserved;
        }
        self.sequence = Some(next);
        Ok(next)
    }
    pub(crate) fn would_materialize(&self, bytes: &[u8]) -> bool {
        crate::workspace::is_automerge_chunk(bytes)
            && self.doc.as_ref().is_some_and(
                |current| {
                    matches!(tilo_automerge::ApplyIncoming::load_incoming(&mut current.clone(), bytes), Ok(n) if n > 0)
                },
            )
    }
    pub(crate) fn materialize(&mut self, bytes: &[u8]) -> bool {
        let Some(current) = self
            .doc
            .as_ref()
            .filter(|_| crate::workspace::is_automerge_chunk(bytes))
        else {
            return false;
        };
        let mut candidate = current.clone();
        if !matches!(tilo_automerge::ApplyIncoming::load_incoming(&mut candidate, bytes), Ok(n) if n > 0)
        {
            return false;
        }
        let saved = candidate.save();
        // Historical best-effort materialization: malformed/opaque bytes do
        // not alter the document; failed snapshot writes do not discard queues.
        let _ = self.store.save_snapshot(&saved);
        self.doc = Some(candidate);
        true
    }
    pub(crate) fn heads(&mut self) -> Vec<[u8; 32]> {
        self.doc
            .as_mut()
            .map(|doc| doc.get_heads().into_iter().map(|h| h.0).collect())
            .unwrap_or_default()
    }
    pub(crate) fn changes(&mut self, heads: &[[u8; 32]]) -> Vec<u8> {
        self.doc
            .as_mut()
            .map(|doc| {
                doc.save_after(
                    &heads
                        .iter()
                        .copied()
                        .map(automerge::ChangeHash)
                        .collect::<Vec<_>>(),
                )
            })
            .unwrap_or_default()
    }
}

// A registry entry owns one runtime for its lifetime: startup bindings are
// installed before serving, and workspace_io::get_or_initialize uses entry-or-
// insert under its reservation. There is no live registry eviction/reopen path.
// Replacing a runtime over an active store is not a supported restart protocol.
pub(crate) async fn handle(workspace: &SharedWorkspace) -> Persistence {
    let mut runtime = workspace.lock().await;
    runtime.persistence.clone().unwrap_or_else(|| {
        let budgets = initial_budgets(&runtime);
        start(&mut runtime, budgets)
    })
}

#[cfg_attr(
    not(test),
    expect(unused_variables, reason = "only test builds read per-runtime budgets")
)]
fn initial_budgets(runtime: &crate::workspace::WorkspaceRuntime) -> AdmissionBudgets {
    #[cfg(test)]
    {
        let budgets = test_budgets()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(runtime.store.root());
        if let Some(budgets) = budgets {
            return budgets;
        }
    }
    AdmissionBudgets::global()
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the owner state is shared with the spawned owner task; the local Arc clone holds no lock"
)]
fn start(
    runtime: &mut crate::workspace::WorkspaceRuntime,
    budgets: AdmissionBudgets,
) -> Persistence {
    let (tx, rx) = mpsc::channel::<NormalJob>(QUEUE_MESSAGES);
    let (replay_tx, replay_rx) = mpsc::channel::<ReplayJob>(REPLAY_MESSAGES);
    let normal_sequence = Arc::new(AtomicU64::new(0));
    let admission = Arc::new(tokio::sync::Mutex::new(()));
    let state = Arc::new(Mutex::new(State {
        store: runtime.store.clone(),
        doc: runtime.doc.take(),
        sequence: None,
        reserved: 0,
        replays: replay::Replays::default(),
        retention_scratch: None,
        retention_candidates: Vec::new(),
        workers: budgets.workers.clone(),
        replay_worker: budgets.replay_worker.clone(),
    }));
    tokio::spawn(run_owner(rx, replay_rx, state));
    let handle = Persistence {
        tx,
        replay_tx,
        normal_sequence,
        admission,
        budgets,
        #[cfg(test)]
        admission_hook: None,
        #[cfg(test)]
        replay_admission_hook: None,
    };
    runtime.persistence = Some(handle.clone());
    handle
}

#[cfg(test)]
pub(crate) fn isolated_test_workers() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(BLOCKING_WORKERS))
}

#[cfg(test)]
pub(crate) fn isolate_test_admission_with_workers(
    runtime: &crate::workspace::WorkspaceRuntime,
    workers: Arc<Semaphore>,
) {
    assert!(
        runtime.persistence.is_none(),
        "test admission must be isolated before persistence starts"
    );
    assert!(
        test_budgets()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                runtime.store.root().to_path_buf(),
                AdmissionBudgets::isolated(workers)
            )
            .is_none(),
        "test admission was configured twice"
    );
}

#[cfg(test)]
fn test_budgets() -> &'static Mutex<std::collections::HashMap<std::path::PathBuf, AdmissionBudgets>>
{
    static BUDGETS: OnceLock<
        Mutex<std::collections::HashMap<std::path::PathBuf, AdmissionBudgets>>,
    > = OnceLock::new();
    BUDGETS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// Idle owner turns per store root, for tests that bound them.
#[cfg(test)]
fn idle_turns() -> &'static Mutex<std::collections::HashMap<std::path::PathBuf, u64>> {
    static TURNS: OnceLock<Mutex<std::collections::HashMap<std::path::PathBuf, u64>>> =
        OnceLock::new();
    TURNS.get_or_init(Mutex::default)
}

#[cfg(test)]
fn note_idle_turn(state: &Mutex<State>) {
    let root = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .store
        .root()
        .to_path_buf();
    idle_turns()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(root)
        .and_modify(|turns| *turns = turns.saturating_add(1))
        .or_insert(1);
}

#[expect(
    clippy::too_many_lines,
    reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
)]
async fn run_owner(
    mut rx: mpsc::Receiver<NormalJob>,
    mut replay_rx: mpsc::Receiver<ReplayJob>,
    state: Arc<Mutex<State>>,
) {
    let workers = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .workers
        .clone();
    // The startup scan is never reused after a new ticket is admitted.
    let mut startup = None;
    loop {
        let shared = state.clone();
        let result = blocking(workers.clone(), move || {
            let state = shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let result = (|| {
                let cursor = match startup.take() {
                    Some(cursor) => cursor,
                    None => state.store.start_work_reclamation()?,
                };
                startup.insert(cursor).step(WorkLimit::default())
            })();
            (startup, result)
        })
        .await;
        // A worker that cannot be acquired or that panicked ends this owner,
        // failing every queued job instead of retrying without a worker.
        let Ok((cursor, result)) = result else {
            tracing::error!("workspace startup reclamation worker unavailable");
            return;
        };
        startup = cursor;
        match result {
            Ok(Step::Done(())) => break,
            Ok(Step::More) => tokio::task::yield_now().await,
            Err(error) => {
                tracing::warn!(%error, "workspace startup reclamation failed");
                tokio::time::sleep(RECLAIM_RETRY).await;
            }
        }
    }
    drop(startup);
    // Idle turns exist only to reclaim replay entries. One becomes
    // reclaimable when its session lets go of its ticket, and dropping the
    // ticket wakes this task (`reclaim_wake`). From then on the owner steps
    // the released entries one per interval, between jobs, until none is
    // left; with none it sleeps until a job or a released ticket arrives.
    let reclaim_wake = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .replays
        .reclaim_wake();
    let mut idle = tokio::time::interval(RECLAIM_INTERVAL);
    idle.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut reclaiming = false;
    let mut prefer_normal = false;
    let mut normal_open = true;
    let mut replay_open = true;
    let mut completed_normal = 0u64;
    let mut pending_replay = None;
    loop {
        enum Work {
            Normal(NormalJob),
            Replay(ReplayJob),
            QueueReplay(ReplayJob),
            Idle,
            Closed,
        }
        if pending_replay.is_none() && replay_open {
            match replay_rx.try_recv() {
                Ok(job) => pending_replay = Some(job),
                Err(mpsc::error::TryRecvError::Disconnected) => replay_open = false,
                Err(mpsc::error::TryRecvError::Empty) => {}
            }
        }
        if !normal_open && !replay_open && pending_replay.is_none() {
            break;
        }
        // A first shell basis may depend on normal jobs already queued when the
        // capture was admitted. Its fixed watermark excludes later traffic, so
        // unrelated continuous input cannot extend this prerequisite window.
        let mut waiting_prerequisite = pending_replay
            .as_ref()
            .and_then(|job| job.prerequisite)
            .is_some_and(|required| required > completed_normal);
        if waiting_prerequisite {
            let shared = state.clone();
            let has_basis = blocking(workers.clone(), move || {
                shared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .store
                    .replay_capture_has_basis()
            })
            .await;
            if matches!(has_basis, Ok(Ok(true))) {
                if let Some(job) = pending_replay.as_mut() {
                    job.prerequisite = None;
                }
                waiting_prerequisite = false;
            }
        }
        let work = if waiting_prerequisite && normal_open {
            if let Some(job) = rx.recv().await {
                Work::Normal(job)
            } else {
                normal_open = false;
                Work::Idle
            }
        } else if prefer_normal && normal_open {
            match rx.try_recv() {
                Ok(job) => Work::Normal(job),
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    normal_open = false;
                    Work::Idle
                }
                Err(mpsc::error::TryRecvError::Empty) => {
                    if let Some(job) = pending_replay.take() {
                        Work::Replay(job)
                    } else {
                        tokio::select! {
                            biased;
                            job = rx.recv(), if normal_open => if let Some(job) = job { Work::Normal(job) } else { normal_open = false; Work::Idle },
                            job = replay_rx.recv(), if replay_open => if let Some(job) = job { Work::QueueReplay(job) } else { replay_open = false; Work::Idle },
                            () = reclaim_wake.notified(), if !reclaiming => Work::Idle,
                            _ = idle.tick(), if reclaiming => Work::Idle,
                        }
                    }
                }
            }
        } else if let Some(job) = pending_replay.take() {
            Work::Replay(job)
        } else if normal_open || replay_open {
            tokio::select! {
                biased;
                job = replay_rx.recv(), if replay_open => if let Some(job) = job { Work::QueueReplay(job) } else { replay_open = false; Work::Idle },
                job = rx.recv(), if normal_open => if let Some(job) = job { Work::Normal(job) } else { normal_open = false; Work::Idle },
                () = reclaim_wake.notified(), if !reclaiming => Work::Idle,
                _ = idle.tick(), if reclaiming => Work::Idle,
            }
        } else {
            Work::Closed
        };
        match work {
            Work::Normal(job) => {
                (job.job)(state.clone()).await;
                completed_normal = completed_normal.max(job.sequence);
                prefer_normal = false;
            }
            Work::Replay(job) => {
                (job.job)(state.clone()).await;
                prefer_normal = true;
            }
            Work::QueueReplay(job) => pending_replay = Some(job),
            Work::Idle => {
                #[cfg(test)]
                note_idle_turn(&state);
            }
            Work::Closed => break,
        }
        // One retained transaction can leave at most one known scratch
        // root. Finish its disposal before another mutation can create one.
        let root = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retention_scratch
            .take();
        if let Some(root) = root {
            let mut discard = None;
            loop {
                let path = root.clone();
                let Ok((next, result)) = blocking(workers.clone(), move || {
                    let result = (|| -> Result<Step<()>, StoreError> {
                        let start = std::time::Instant::now();
                        let cursor = match discard.take() {
                            Some(cursor) => cursor,
                            None => crate::store::replay::WorkDiscard::new(path)?,
                        };
                        let cursor = discard.insert(cursor);
                        loop {
                            let step = cursor.step(WorkLimit::default())?;
                            if step != Step::More || start.elapsed() >= SLICE {
                                return Ok(step);
                            }
                        }
                    })();
                    (discard, result)
                })
                .await
                else {
                    tracing::error!("retention disposal worker unavailable");
                    return;
                };
                discard = next;
                match result {
                    Ok(Step::Done(())) => break,
                    Ok(Step::More) => tokio::task::yield_now().await,
                    Err(error) => {
                        tracing::warn!(%error, "retention disposal failed");
                        tokio::time::sleep(RECLAIM_RETRY).await;
                    }
                }
            }
        }
        let candidates = std::mem::take(
            &mut state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retention_candidates,
        );
        for (lineage, staged) in candidates {
            let mut discard = None;
            loop {
                let lineage = lineage.clone();
                let staged = staged.clone();
                let Ok((next, result)) = blocking(workers.clone(), move || {
                    let result = (|| {
                        if discard.is_none() {
                            discard =
                                crate::store::retained::CheckpointDiscard::new(&lineage, staged)?;
                        }
                        match discard.as_mut() {
                            Some(discard) => discard.step(WorkLimit::default()),
                            None => Ok(Step::Done(())),
                        }
                    })();
                    (discard, result)
                })
                .await
                else {
                    tracing::error!("checkpoint stage disposal worker unavailable");
                    return;
                };
                discard = next;
                match result {
                    Ok(Step::Done(())) => break,
                    Ok(Step::More) => tokio::task::yield_now().await,
                    Err(error) => {
                        tracing::warn!(%error, "checkpoint stage disposal failed");
                        tokio::time::sleep(RECLAIM_RETRY).await;
                    }
                }
            }
        }
        // An entry still in use needs no turn: its ticket's drop wakes the
        // owner. So does an entry whose session or connection ended, since
        // that session drops its ticket as it ends.
        if !state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replays
            .reclaimable()
        {
            reclaiming = false;
            continue;
        }
        if !reclaiming {
            // The first idle turn comes an interval from now, not at once
            // for every interval missed while there was nothing to reclaim.
            idle.reset();
            reclaiming = true;
        }
        let shared = state.clone();
        let _ = blocking(workers.clone(), move || {
            let mut state = shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Err(error) = state.reclaim_replay_step() {
                tracing::warn!(%error, "replay reclamation failed");
            }
        })
        .await;
    }
}

impl Persistence {
    pub(crate) fn workers(&self) -> Arc<Semaphore> {
        self.budgets.workers.clone()
    }

    /// Nonwaiting admission: includes queued and executing work in both bounds.
    /// `bytes` charges owned input, encoding and decoded coedit body. Replay
    /// batches retain their separate process-wide output permits through writes.
    pub(crate) fn submit<T, F, C, CF>(
        &self,
        bytes: usize,
        operation: F,
        completion: C,
    ) -> Result<oneshot::Receiver<Result<T, StoreError>>, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut State) -> Result<T, StoreError> + Send + 'static,
        C: FnOnce(Result<T, StoreError>) -> CF + Send + 'static,
        CF: Future<Output = Result<T, StoreError>> + Send + 'static,
    {
        self.submit_prepared(
            bytes,
            async { Ok(()) },
            move |state, ()| operation(state),
            completion,
        )
    }

    /// Preparation runs in owner order, after the preceding publication and
    /// before blocking persistence. Its result must not retain routing guards.
    pub(crate) fn submit_prepared<P, PF, T, F, C, CF>(
        &self,
        bytes: usize,
        preparation: PF,
        operation: F,
        completion: C,
    ) -> Result<oneshot::Receiver<Result<T, StoreError>>, StoreError>
    where
        P: Send + 'static,
        PF: Future<Output = Result<P, StoreError>> + Send + 'static,
        T: Send + 'static,
        F: FnOnce(&mut State, P) -> Result<T, StoreError> + Send + 'static,
        C: FnOnce(Result<T, StoreError>) -> CF + Send + 'static,
        CF: Future<Output = Result<T, StoreError>> + Send + 'static,
    {
        let mut operation = Some(operation);
        self.submit_resumable(
            bytes,
            preparation,
            move |state, prepared| {
                let operation = operation
                    .take()
                    .invariant("single-step operation already ran")?;
                let prepared = prepared.take().invariant("preparation already consumed")?;
                operation(state, prepared).map(Step::Done)
            },
            completion,
        )
    }

    /// Wait for bounded owner capacity before an ordered single-step operation.
    /// Dropping admission releases its permits; admitted work still completes.
    pub(crate) async fn submit_prepared_wait<P, PF, T, F, C, CF>(
        &self,
        bytes: usize,
        preparation: PF,
        operation: F,
        completion: C,
    ) -> Result<oneshot::Receiver<Result<T, StoreError>>, StoreError>
    where
        P: Send + 'static,
        PF: Future<Output = Result<P, StoreError>> + Send + 'static,
        T: Send + 'static,
        F: FnOnce(&mut State, P) -> Result<T, StoreError> + Send + 'static,
        C: FnOnce(Result<T, StoreError>) -> CF + Send + 'static,
        CF: Future<Output = Result<T, StoreError>> + Send + 'static,
    {
        let mut operation = Some(operation);
        self.submit_resumable_wait(
            bytes,
            preparation,
            move |state, prepared| {
                let operation = operation
                    .take()
                    .invariant("single-step operation already ran")?;
                let prepared = prepared.take().invariant("preparation already consumed")?;
                operation(state, prepared).map(Step::Done)
            },
            completion,
        )
        .await
    }

    /// Keep the admitted transaction's owner position across fixed quanta, but
    /// return the global blocking worker before awaiting the next quantum.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the admission permits move into the job; the lint does not see the move"
    )]
    pub(crate) fn submit_resumable<P, PF, T, F, C, CF>(
        &self,
        bytes: usize,
        preparation: PF,
        operation: F,
        completion: C,
    ) -> Result<oneshot::Receiver<Result<T, StoreError>>, StoreError>
    where
        P: Send + 'static,
        PF: Future<Output = Result<P, StoreError>> + Send + 'static,
        T: Send + 'static,
        F: FnMut(&mut State, &mut Option<P>) -> Result<Step<T>, StoreError> + Send + 'static,
        C: FnOnce(Result<T, StoreError>) -> CF + Send + 'static,
        CF: Future<Output = Result<T, StoreError>> + Send + 'static,
    {
        let slot = self
            .budgets
            .normal_messages
            .clone()
            .try_acquire_owned()
            .map_err(|_| error("state message backpressure"))?;
        let byte_count = charged_bytes(bytes)?;
        let bytes = self
            .budgets
            .normal_bytes
            .clone()
            .try_acquire_many_owned(byte_count)
            .map_err(|_| error("state byte backpressure"))?;
        let (job, rx) = resumable_job(slot, bytes, byte_count, preparation, operation, completion);
        let permit = self
            .tx
            .try_reserve()
            .map_err(|_| error("workspace state backpressure"))?;
        let _admission = self
            .admission
            .try_lock()
            .map_err(|_| error("workspace state backpressure"))?;
        let sequence = next_normal_job(&self.normal_sequence)?;
        #[cfg(test)]
        if let Some(hook) = &self.admission_hook {
            hook();
        }
        permit.send(NormalJob { sequence, job });
        Ok(rx)
    }

    /// Durable state and replay apply capacity-driven backpressure instead of
    /// turning a healthy replay burst into a connection error.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the admission permits move into the job; the lint does not see the move"
    )]
    pub(crate) async fn submit_resumable_wait<P, PF, T, F, C, CF>(
        &self,
        bytes: usize,
        preparation: PF,
        operation: F,
        completion: C,
    ) -> Result<oneshot::Receiver<Result<T, StoreError>>, StoreError>
    where
        P: Send + 'static,
        PF: Future<Output = Result<P, StoreError>> + Send + 'static,
        T: Send + 'static,
        F: FnMut(&mut State, &mut Option<P>) -> Result<Step<T>, StoreError> + Send + 'static,
        C: FnOnce(Result<T, StoreError>) -> CF + Send + 'static,
        CF: Future<Output = Result<T, StoreError>> + Send + 'static,
    {
        let byte_count = charged_bytes(bytes)?;
        let slot = self
            .budgets
            .normal_messages
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| error("state persistence unavailable"))?;
        let bytes = self
            .budgets
            .normal_bytes
            .clone()
            .acquire_many_owned(byte_count)
            .await
            .map_err(|_| error("state persistence unavailable"))?;
        let (job, rx) = resumable_job(slot, bytes, byte_count, preparation, operation, completion);
        let permit = self
            .tx
            .reserve()
            .await
            .map_err(|_| error("state persistence unavailable"))?;
        let _admission = self.admission.lock().await;
        let sequence = next_normal_job(&self.normal_sequence)?;
        #[cfg(test)]
        if let Some(hook) = &self.admission_hook {
            hook();
        }
        permit.send(NormalJob { sequence, job });
        Ok(rx)
    }

    /// Admit one ordered owner job on behalf of a bounded input sequence.
    /// The job consumes the same normal message and byte capacity as its
    /// individual inputs, but holds one queue position through completion.
    pub(crate) async fn submit_sequence_wait<T, Run, Fut>(
        &self,
        message_units: u32,
        charged_size: usize,
        run: Run,
    ) -> Result<oneshot::Receiver<Result<T, StoreError>>, StoreError>
    where
        T: Send + 'static,
        Run: FnOnce(Arc<Mutex<State>>, Arc<Semaphore>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, StoreError>> + Send + 'static,
    {
        if message_units == 0
            || usize::try_from(message_units)
                .ok()
                .is_none_or(|units| units > NORMAL_MESSAGES)
        {
            return Err(error("state message backpressure"));
        }
        let byte_count = charged_bytes(charged_size)?;
        let slots = self
            .budgets
            .normal_messages
            .clone()
            .acquire_many_owned(message_units)
            .await
            .map_err(|_| error("state persistence unavailable"))?;
        let bytes = self
            .budgets
            .normal_bytes
            .clone()
            .acquire_many_owned(byte_count)
            .await
            .map_err(|_| error("state persistence unavailable"))?;
        let (tx, rx) = oneshot::channel();
        let job = Box::new(
            move |state: Arc<Mutex<State>>| -> Pin<Box<dyn Future<Output = ()> + Send>> {
                Box::pin(async move {
                    let (_slots, _bytes) = (slots, bytes);
                    let workers = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .workers
                        .clone();
                    let result = run(state, workers).await;
                    let _ = tx.send(result);
                })
            },
        );
        let permit = self
            .tx
            .reserve()
            .await
            .map_err(|_| error("state persistence unavailable"))?;
        let _admission = self.admission.lock().await;
        let sequence = next_normal_job(&self.normal_sequence)?;
        #[cfg(test)]
        if let Some(hook) = &self.admission_hook {
            hook();
        }
        permit.send(NormalJob { sequence, job });
        Ok(rx)
    }

    /// Replay capture and cleanup reserve bounded admission inside the existing
    /// global totals so queued mutation input cannot strand a fresh subscriber.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the admission permits move into the job; the lint does not see the move"
    )]
    pub(crate) async fn submit_replay_resumable_wait<P, PF, T, F, C, CF>(
        &self,
        capture: bool,
        bytes: usize,
        preparation: PF,
        operation: F,
        completion: C,
    ) -> Result<oneshot::Receiver<Result<T, StoreError>>, StoreError>
    where
        P: Send + 'static,
        PF: Future<Output = Result<P, StoreError>> + Send + 'static,
        T: Send + 'static,
        F: FnMut(&mut State, &mut Option<P>) -> Result<Step<T>, StoreError> + Send + 'static,
        C: FnOnce(Result<T, StoreError>) -> CF + Send + 'static,
        CF: Future<Output = Result<T, StoreError>> + Send + 'static,
    {
        let byte_count = u32::try_from(bytes.max(1))
            .ok()
            .filter(|bytes| *bytes as usize <= REPLAY_BYTES)
            .ok_or_else(|| error("replay state byte backpressure"))?;
        let slot = self
            .budgets
            .replay_messages
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| error("state persistence unavailable"))?;
        let bytes = self
            .budgets
            .replay_bytes
            .clone()
            .acquire_many_owned(byte_count)
            .await
            .map_err(|_| error("state persistence unavailable"))?;
        let (job, rx) = resumable_job(slot, bytes, byte_count, preparation, operation, completion);
        let _admission = self.admission.lock().await;
        let prerequisite = capture.then(|| self.normal_sequence.load(Ordering::Acquire));
        #[cfg(test)]
        if let Some(hook) = &self.replay_admission_hook {
            hook();
        }
        self.replay_tx
            .send(ReplayJob { prerequisite, job })
            .await
            .map_err(|_| error("state persistence unavailable"))?;
        Ok(rx)
    }

    pub(crate) fn read<T: Send + 'static>(
        &self,
        bytes: usize,
        operation: impl FnOnce(&mut State) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<oneshot::Receiver<Result<T, StoreError>>, StoreError> {
        self.submit(bytes, operation, |result| async { result })
    }
}

fn resumable_job<P, PF, T, F, C, CF>(
    slot: OwnedSemaphorePermit,
    bytes: OwnedSemaphorePermit,
    byte_count: u32,
    preparation: PF,
    operation: F,
    completion: C,
) -> (Job, oneshot::Receiver<Result<T, StoreError>>)
where
    P: Send + 'static,
    PF: Future<Output = Result<P, StoreError>> + Send + 'static,
    T: Send + 'static,
    F: FnMut(&mut State, &mut Option<P>) -> Result<Step<T>, StoreError> + Send + 'static,
    C: FnOnce(Result<T, StoreError>) -> CF + Send + 'static,
    CF: Future<Output = Result<T, StoreError>> + Send + 'static,
{
    let (tx, rx) = oneshot::channel();
    let queued = std::time::Instant::now();
    // The operation and its preparation go into and out of the job's box and
    // the blocking pool on every quantum: on the heap, each move is a pointer.
    let mut operation = Box::new(operation);
    let preparation = Box::pin(preparation);
    let job = Box::new(
        move |state: Arc<Mutex<State>>| -> Pin<Box<dyn Future<Output = ()> + Send>> {
            Box::pin(async move {
                let (_slot, _bytes) = (slot, bytes);
                let workers = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .workers
                    .clone();
                let result = match preparation.await {
                    Err(error) => Err(error),
                    Ok(prepared) => {
                        let mut prepared = Some(prepared);
                        loop {
                            let shared = state.clone();
                            let quantum = blocking(workers.clone(), move || {
                                let start = std::time::Instant::now();
                                let mut state = shared
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                let result = loop {
                                    let result = operation(&mut state, &mut prepared);
                                    if !matches!(result, Ok(Step::More)) || start.elapsed() >= SLICE
                                    {
                                        break result;
                                    }
                                };
                                drop(state);
                                if diagnostics() {
                                    tracing::info!(
                                        phase = "state_persist",
                                        charged_bytes = byte_count,
                                        queue_us =
                                            u64::try_from(start.duration_since(queued).as_micros())
                                                .unwrap_or(u64::MAX),
                                        persist_us = u64::try_from(start.elapsed().as_micros())
                                            .unwrap_or(u64::MAX)
                                    );
                                }
                                (operation, prepared, result)
                            })
                            .await;
                            let (next_operation, next_prepared, result) = match quantum {
                                Ok(value) => value,
                                Err(error) => break Err(error),
                            };
                            operation = next_operation;
                            prepared = next_prepared;
                            match result {
                                Ok(Step::More) => tokio::task::yield_now().await,
                                Ok(Step::Done(value)) => break Ok(value),
                                Err(error) => break Err(error),
                            }
                        }
                    }
                };
                // The owner serializes this asynchronous authorization/publication
                // step as well as the blocking transaction, even if tx is closed.
                let result = completion(result).await;
                let _ = tx.send(result);
            })
        },
    );
    (job, rx)
}
pub(crate) async fn receipt<T>(
    rx: oneshot::Receiver<Result<T, StoreError>>,
) -> Result<T, StoreError> {
    rx.await.map_err(|e| error(e.to_string()))?
}
pub(crate) fn diagnostics() -> bool {
    std::env::var_os("TILO_RELAY_PHASE_DIAGNOSTICS").is_some()
}

/// Low-volume payload-free event correlation, independent of per-quantum logs.
pub(crate) fn correlated_diagnostics() -> bool {
    std::env::var_os("TILO_RELAY_CORRELATED_DIAGNOSTICS").is_some()
}

/// Shared live deliveries charge their single Arc payload once process-wide.
pub(crate) fn delivery_budget(
    bytes: usize,
) -> Result<Arc<tokio::sync::OwnedSemaphorePermit>, StoreError> {
    static LIVE: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let bytes = u32::try_from(bytes.max(1)).map_err(|_| error("live state byte backpressure"))?;
    LIVE.get_or_init(|| Arc::new(Semaphore::new(128 * 1024 * 1024)))
        .clone()
        .try_acquire_many_owned(bytes)
        .map(Arc::new)
        .map_err(|_| error("live state byte backpressure"))
}

fn blocking_budget() -> &'static Arc<Semaphore> {
    static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    WORKERS.get_or_init(|| Arc::new(Semaphore::new(BLOCKING_WORKERS)))
}

pub(crate) async fn blocking<T: Send + 'static>(
    workers: Arc<Semaphore>,
    operation: impl FnOnce() -> T + Send + 'static,
) -> Result<T, StoreError> {
    let permit = workers
        .acquire_owned()
        .await
        .map_err(|e| error(e.to_string()))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    })
    .await
    .map_err(|e| error(e.to_string()))
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
#[allow(
    clippy::too_many_lines,
    reason = "each test spells out one end-to-end scenario"
)]
mod tests {
    use super::*;

    fn test_persistence(
        root: &std::path::Path,
        budgets: AdmissionBudgets,
    ) -> (Persistence, tokio::task::JoinHandle<()>) {
        let state = state(root);
        let (tx, rx) = mpsc::channel::<NormalJob>(8);
        let (replay_tx, replay_rx) = mpsc::channel::<ReplayJob>(8);
        let owner = Persistence {
            tx,
            replay_tx,
            normal_sequence: Arc::new(AtomicU64::new(0)),
            admission: Arc::new(tokio::sync::Mutex::new(())),
            budgets,
            admission_hook: None,
            replay_admission_hook: None,
        };
        let running = tokio::spawn(run_owner(rx, replay_rx, state));
        (owner, running)
    }

    fn state(root: &std::path::Path) -> Arc<Mutex<State>> {
        Arc::new(Mutex::new(State {
            store: WorkspaceStore::open(root, 7, false).unwrap(),
            doc: None,
            sequence: None,
            reserved: 0,
            replays: replay::Replays::default(),
            retention_scratch: None,
            retention_candidates: Vec::new(),
            workers: isolated_test_workers(),
            replay_worker: Arc::new(Semaphore::new(1)),
        }))
    }

    fn record_job(order: Arc<Mutex<Vec<&'static str>>>, value: &'static str) -> Job {
        Box::new(move |_| {
            Box::pin(async move {
                order
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(value);
            })
        })
    }

    fn idle_turns_of(root: &std::path::Path) -> u64 {
        super::idle_turns()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(root)
            .copied()
            .unwrap_or(0)
    }

    async fn store_root(owner: &Persistence) -> std::path::PathBuf {
        receipt(
            owner
                .read(1, |state| Ok(state.store.root().to_path_buf()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_owner_does_not_tick_without_replays_to_reclaim() {
        let dir = tempfile::tempdir().unwrap();
        let (owner, _running) = test_persistence(
            dir.path(),
            AdmissionBudgets::isolated(isolated_test_workers()),
        );
        // Startup reclamation is done once a first job has run.
        let root = store_root(&owner).await;
        let before = idle_turns_of(&root);
        tokio::time::sleep(RECLAIM_INTERVAL * 25).await;
        assert_eq!(
            idle_turns_of(&root) - before,
            0,
            "an owner with nothing to reclaim must sleep until work arrives"
        );
        let workspace = receipt(
            owner
                .read(1, |state| Ok(state.store.workspace_id()))
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(workspace, 7, "the sleeping owner still runs the next job");
    }

    async fn captured_replay(owner: &Persistence) -> ReplayTicket {
        let ticket = ReplayTicket::reserve([2; 32], None).unwrap();
        let authority = ticket.authority.clone();
        receipt(
            owner
                .read(1, move |state| {
                    loop {
                        let step = state.capture_replay(&authority)?;
                        if matches!(step, Step::Done(_)) {
                            return Ok(());
                        }
                    }
                })
                .unwrap(),
        )
        .await
        .unwrap();
        ticket
    }

    #[tokio::test(start_paused = true)]
    async fn an_owner_with_a_reclaimable_replay_keeps_reclaiming() {
        let dir = tempfile::tempdir().unwrap();
        let (owner, _running) = test_persistence(
            dir.path(),
            AdmissionBudgets::isolated(isolated_test_workers()),
        );
        let root = store_root(&owner).await;
        let ticket = captured_replay(&owner).await;
        // The session lets go of its ticket outside the owner; only the
        // owner's own idle turns can reclaim the entry, without a job.
        drop(ticket);
        let before = idle_turns_of(&root);
        tokio::time::sleep(RECLAIM_INTERVAL * 25).await;
        assert!(
            idle_turns_of(&root) > before,
            "a released ticket wakes its owner to reclaim the entry"
        );
        let reclaimed = receipt(owner.read(1, |state| Ok(state.replays.is_empty())).unwrap())
            .await
            .unwrap();
        assert!(reclaimed, "idle turns reclaimed the released replay");
    }

    #[tokio::test(start_paused = true)]
    async fn an_owner_whose_replays_are_in_use_does_not_tick() {
        let dir = tempfile::tempdir().unwrap();
        let (owner, _running) = test_persistence(
            dir.path(),
            AdmissionBudgets::isolated(isolated_test_workers()),
        );
        let root = store_root(&owner).await;
        let ticket = captured_replay(&owner).await;
        let before = idle_turns_of(&root);
        tokio::time::sleep(RECLAIM_INTERVAL * 25).await;
        assert_eq!(
            idle_turns_of(&root) - before,
            0,
            "an owner whose replay entries are all in use sleeps until one is released"
        );
        let held = receipt(owner.read(1, |state| Ok(state.replays.is_empty())).unwrap())
            .await
            .unwrap();
        assert!(!held, "the entry in use stays in the table");
        drop(ticket);
        tokio::time::sleep(RECLAIM_INTERVAL * 25).await;
        let reclaimed = receipt(owner.read(1, |state| Ok(state.replays.is_empty())).unwrap())
            .await
            .unwrap();
        assert!(
            reclaimed,
            "releasing the ticket woke the owner to reclaim it"
        );
    }

    #[tokio::test]
    async fn sequence_admission_rejects_invalid_message_units() {
        let tmp = tempfile::tempdir().unwrap();
        let (owner, running) = test_persistence(
            tmp.path(),
            AdmissionBudgets::isolated(isolated_test_workers()),
        );

        for units in [0, u32::try_from(NORMAL_MESSAGES).unwrap() + 1] {
            assert!(
                owner
                    .submit_sequence_wait(units, 1, |_, _| async { Ok(()) })
                    .await
                    .is_err(),
                "accepted invalid aggregate message charge {units}"
            );
        }

        drop(owner);
        running.await.unwrap();
    }

    #[tokio::test]
    async fn sequence_admission_holds_aggregate_budgets_until_run_finishes() {
        let tmp = tempfile::tempdir().unwrap();
        let budgets = AdmissionBudgets::isolated(isolated_test_workers());
        let inspect = budgets.clone();
        let (owner, running) = test_persistence(tmp.path(), budgets);
        let started = Arc::new(tokio::sync::Notify::new());
        let signal = started.clone();
        let (release_tx, release_rx) = oneshot::channel();

        let receipt = owner
            .submit_sequence_wait(3, 7, move |_, _| async move {
                signal.notify_one();
                let _ = release_rx.await;
                Ok(9_u8)
            })
            .await
            .unwrap();
        started.notified().await;
        assert_eq!(
            inspect.normal_messages.available_permits(),
            NORMAL_MESSAGES - 3
        );
        assert_eq!(inspect.normal_bytes.available_permits(), NORMAL_BYTES - 7);
        let remaining_messages = inspect
            .normal_messages
            .clone()
            .try_acquire_many_owned(u32::try_from(NORMAL_MESSAGES - 3).unwrap())
            .unwrap();
        assert!(
            owner
                .submit_resumable(
                    1,
                    async { Ok(()) },
                    |_, _| Ok(Step::Done(())),
                    |result| async { result },
                )
                .is_err(),
            "ordinary admission bypassed the aggregate message charge"
        );

        release_tx.send(()).unwrap();
        assert_eq!(crate::persistence::receipt(receipt).await.unwrap(), 9);
        tokio::task::yield_now().await;
        assert_eq!(inspect.normal_messages.available_permits(), 3);
        assert_eq!(inspect.normal_bytes.available_permits(), NORMAL_BYTES);
        drop(remaining_messages);
        assert_eq!(inspect.normal_messages.available_permits(), NORMAL_MESSAGES);

        drop(owner);
        running.await.unwrap();
        assert_eq!(inspect.normal_messages.available_permits(), NORMAL_MESSAGES);
        assert_eq!(inspect.normal_bytes.available_permits(), NORMAL_BYTES);
    }

    #[tokio::test]
    async fn sequence_admission_completes_after_receiver_cancellation() {
        let tmp = tempfile::tempdir().unwrap();
        let budgets = AdmissionBudgets::isolated(isolated_test_workers());
        let inspect = budgets.clone();
        let (owner, running) = test_persistence(tmp.path(), budgets);
        let started = Arc::new(tokio::sync::Notify::new());
        let signal = started.clone();
        let completed = Arc::new(tokio::sync::Notify::new());
        let finished = completed.clone();
        let (release_tx, release_rx) = oneshot::channel();

        let receipt = owner
            .submit_sequence_wait(2, 5, move |_, _| async move {
                signal.notify_one();
                let _ = release_rx.await;
                finished.notify_one();
                Ok(())
            })
            .await
            .unwrap();
        started.notified().await;
        drop(receipt);
        release_tx.send(()).unwrap();
        tilo_test_env::within(
            "cancelled receiver stopped admitted sequence work",
            completed.notified(),
        )
        .await;
        tokio::task::yield_now().await;
        assert_eq!(inspect.normal_messages.available_permits(), NORMAL_MESSAGES);
        assert_eq!(inspect.normal_bytes.available_permits(), NORMAL_BYTES);

        drop(owner);
        running.await.unwrap();
    }

    #[tokio::test]
    async fn sequence_admission_leaves_replay_reserve_available() {
        let tmp = tempfile::tempdir().unwrap();
        let budgets = AdmissionBudgets::isolated(isolated_test_workers());
        let (owner, running) = test_persistence(tmp.path(), budgets);
        let started = Arc::new(tokio::sync::Notify::new());
        let signal = started.clone();
        let (release_tx, release_rx) = oneshot::channel();

        let sequence = owner
            .submit_sequence_wait(
                u32::try_from(NORMAL_MESSAGES).unwrap(),
                NORMAL_BYTES,
                move |_, _| async move {
                    signal.notify_one();
                    let _ = release_rx.await;
                    Ok(())
                },
            )
            .await
            .unwrap();
        started.notified().await;
        let replay = tilo_test_env::within(
            "saturated normal sequence blocked reserved replay admission",
            owner.submit_replay_resumable_wait(
                false,
                1,
                async { Ok(()) },
                |_, _| Ok(Step::Done(())),
                |result| async { result },
            ),
        )
        .await
        .unwrap();

        release_tx.send(()).unwrap();
        crate::persistence::receipt(sequence).await.unwrap();
        crate::persistence::receipt(replay).await.unwrap();
        drop(owner);
        running.await.unwrap();
    }

    #[tokio::test]
    async fn sequence_admission_uses_one_ordered_queue_position() {
        let tmp = tempfile::tempdir().unwrap();
        let (owner, running) = test_persistence(
            tmp.path(),
            AdmissionBudgets::isolated(isolated_test_workers()),
        );
        let order = Arc::new(Mutex::new(Vec::new()));
        let sequence_order = order.clone();
        let sequence = owner
            .submit_sequence_wait(4, 4, move |state, workers| async move {
                let owner_workers = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .workers
                    .clone();
                assert!(Arc::ptr_eq(&workers, &owner_workers));
                sequence_order.lock().unwrap().push("sequence");
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(owner.normal_sequence.load(Ordering::Acquire), 1);
        let normal_order = order.clone();
        let normal = owner
            .submit_resumable_wait(
                1,
                async { Ok(()) },
                move |_, _| {
                    normal_order.lock().unwrap().push("normal");
                    Ok(Step::Done(()))
                },
                |result| async { result },
            )
            .await
            .unwrap();
        assert_eq!(owner.normal_sequence.load(Ordering::Acquire), 2);

        crate::persistence::receipt(sequence).await.unwrap();
        crate::persistence::receipt(normal).await.unwrap();
        assert_eq!(*order.lock().unwrap(), ["sequence", "normal"]);
        drop(owner);
        running.await.unwrap();
    }

    #[tokio::test]
    async fn replay_reserve_remains_available_when_normal_budgets_are_saturated() {
        assert_eq!(NORMAL_MESSAGES + REPLAY_MESSAGES, GLOBAL_MESSAGES);
        assert_eq!(NORMAL_BYTES + REPLAY_BYTES, GLOBAL_BYTES);
        let budgets = AdmissionBudgets {
            normal_messages: Arc::new(Semaphore::new(NORMAL_MESSAGES)),
            normal_bytes: Arc::new(Semaphore::new(NORMAL_BYTES)),
            replay_messages: Arc::new(Semaphore::new(REPLAY_MESSAGES)),
            replay_bytes: Arc::new(Semaphore::new(REPLAY_BYTES)),
            workers: isolated_test_workers(),
            replay_worker: Arc::new(Semaphore::new(1)),
        };
        let _normal_message_hold = budgets
            .normal_messages
            .clone()
            .acquire_many_owned(u32::try_from(NORMAL_MESSAGES).unwrap())
            .await
            .unwrap();
        let _normal_byte_hold = budgets
            .normal_bytes
            .clone()
            .acquire_many_owned(u32::try_from(NORMAL_BYTES).unwrap())
            .await
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let (normal_tx, normal_rx) = mpsc::channel::<NormalJob>(8);
        let (replay_tx, replay_rx) = mpsc::channel::<ReplayJob>(8);
        let owner = Persistence {
            tx: normal_tx,
            replay_tx,
            normal_sequence: Arc::new(AtomicU64::new(0)),
            admission: Arc::new(tokio::sync::Mutex::new(())),
            budgets,
            admission_hook: None,
            replay_admission_hook: None,
        };
        let running = tokio::spawn(run_owner(normal_rx, replay_rx, state));
        assert!(
            owner
                .submit_resumable(
                    1,
                    async { Ok(()) },
                    |_, _| Ok(Step::Done(())),
                    |result| async { result },
                )
                .is_err(),
            "normal admission ignored its saturated production budget"
        );
        let replay = owner
            .submit_replay_resumable_wait(
                false,
                1,
                async { Ok(()) },
                |_, _| Ok(Step::Done(())),
                |result| async { result },
            )
            .await
            .expect("reserved replay admission used the saturated normal budget");
        receipt(replay).await.unwrap();
        drop(owner);
        running.await.unwrap();
    }

    #[tokio::test]
    async fn owner_drains_both_closed_queues_and_prioritizes_one_replay_after_current_normal() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let (normal_tx, normal_rx) = mpsc::channel::<NormalJob>(8);
        let (replay_tx, replay_rx) = mpsc::channel::<ReplayJob>(8);
        let order = Arc::new(Mutex::new(Vec::new()));
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let started = Arc::new(tokio::sync::Notify::new());
        let signal = started.clone();
        normal_tx
            .send(NormalJob {
                sequence: 1,
                job: Box::new(move |_| {
                    Box::pin(async move {
                        signal.notify_one();
                        let _ = release_rx.await;
                    })
                }),
            })
            .await
            .unwrap();
        let running = tokio::spawn(run_owner(normal_rx, replay_rx, state));
        started.notified().await;
        normal_tx
            .send(NormalJob {
                sequence: 2,
                job: record_job(order.clone(), "normal-1"),
            })
            .await
            .unwrap();
        normal_tx
            .send(NormalJob {
                sequence: 3,
                job: record_job(order.clone(), "normal-2"),
            })
            .await
            .unwrap();
        replay_tx
            .send(ReplayJob {
                prerequisite: None,
                job: record_job(order.clone(), "replay-1"),
            })
            .await
            .unwrap();
        replay_tx
            .send(ReplayJob {
                prerequisite: None,
                job: record_job(order.clone(), "replay-2"),
            })
            .await
            .unwrap();
        drop(normal_tx);
        drop(replay_tx);
        release_tx.send(()).unwrap();
        tilo_test_env::within("closed queues were not drained", running)
            .await
            .unwrap();
        assert_eq!(
            *order
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            ["replay-1", "normal-1", "replay-2", "normal-2"]
        );
    }

    #[tokio::test]
    async fn first_basis_prerequisites_run_before_priority_capture() {
        let tmp = tempfile::tempdir().unwrap();
        let authority = crate::store::shell_proof_tests::binding();
        let workspace = crate::room_workspace_id(authority.room()).unwrap();
        let mut store = WorkspaceStore::open(tmp.path(), workspace, false).unwrap();
        store.bind_shell_authority(authority).unwrap();
        let state = Arc::new(Mutex::new(State {
            store,
            doc: None,
            sequence: None,
            reserved: 0,
            replays: replay::Replays::default(),
            retention_scratch: None,
            retention_candidates: Vec::new(),
            workers: isolated_test_workers(),
            replay_worker: Arc::new(Semaphore::new(1)),
        }));
        let (normal_tx, normal_rx) = mpsc::channel::<NormalJob>(8);
        let (replay_tx, replay_rx) = mpsc::channel::<ReplayJob>(8);
        let order = Arc::new(Mutex::new(Vec::new()));
        let normal_order = order.clone();
        normal_tx
            .send(NormalJob {
                sequence: 1,
                job: Box::new(move |state| {
                    Box::pin(async move {
                        let root = state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .store
                            .root()
                            .join(crate::store::shell_selection::DIRECTORY);
                        let generation = "00000000000000000000000000000001";
                        std::fs::create_dir_all(root.join(generation)).unwrap();
                        std::fs::write(root.join("CURRENT"), generation).unwrap();
                        normal_order.lock().unwrap().push("normal-basis");
                    })
                }),
            })
            .await
            .unwrap();
        replay_tx
            .send(ReplayJob {
                prerequisite: Some(1),
                job: record_job(order.clone(), "replay-capture"),
            })
            .await
            .unwrap();
        drop(normal_tx);
        drop(replay_tx);
        Box::pin(run_owner(normal_rx, replay_rx, state)).await;
        assert_eq!(*order.lock().unwrap(), ["normal-basis", "replay-capture"]);
    }

    #[tokio::test]
    async fn first_basis_capture_waits_only_for_its_fixed_normal_watermark() {
        let tmp = tempfile::tempdir().unwrap();
        let authority = crate::store::shell_proof_tests::binding();
        let workspace = crate::room_workspace_id(authority.room()).unwrap();
        let mut store = WorkspaceStore::open(tmp.path(), workspace, false).unwrap();
        store.bind_shell_authority(authority).unwrap();
        let state = Arc::new(Mutex::new(State {
            store,
            doc: None,
            sequence: None,
            reserved: 0,
            replays: replay::Replays::default(),
            retention_scratch: None,
            retention_candidates: Vec::new(),
            workers: isolated_test_workers(),
            replay_worker: Arc::new(Semaphore::new(1)),
        }));
        let (normal_tx, normal_rx) = mpsc::channel::<NormalJob>(8);
        let (replay_tx, replay_rx) = mpsc::channel::<ReplayJob>(8);
        let order = Arc::new(Mutex::new(Vec::new()));
        normal_tx
            .send(NormalJob {
                sequence: 1,
                job: record_job(order.clone(), "normal-prerequisite"),
            })
            .await
            .unwrap();
        replay_tx
            .send(ReplayJob {
                prerequisite: Some(1),
                job: record_job(order.clone(), "replay-capture"),
            })
            .await
            .unwrap();
        for (sequence, label) in [(2, "normal-later-1"), (3, "normal-later-2")] {
            normal_tx
                .send(NormalJob {
                    sequence,
                    job: record_job(order.clone(), label),
                })
                .await
                .unwrap();
        }
        drop(normal_tx);
        drop(replay_tx);
        Box::pin(run_owner(normal_rx, replay_rx, state)).await;
        assert_eq!(
            *order.lock().unwrap(),
            [
                "normal-prerequisite",
                "replay-capture",
                "normal-later-1",
                "normal-later-2",
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn production_admission_linearizes_normal_prefix_before_capture_watermark() {
        let tmp = tempfile::tempdir().unwrap();
        let authority = crate::store::shell_proof_tests::binding();
        let workspace = crate::room_workspace_id(authority.room()).unwrap();
        let mut store = WorkspaceStore::open(tmp.path(), workspace, false).unwrap();
        store.bind_shell_authority(authority).unwrap();
        let state = Arc::new(Mutex::new(State {
            store,
            doc: None,
            sequence: None,
            reserved: 0,
            replays: replay::Replays::default(),
            retention_scratch: None,
            retention_candidates: Vec::new(),
            workers: isolated_test_workers(),
            replay_worker: Arc::new(Semaphore::new(1)),
        }));
        let (normal_tx, normal_rx) = mpsc::channel::<NormalJob>(8);
        let (replay_tx, replay_rx) = mpsc::channel::<ReplayJob>(8);
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let admitted = Arc::new(tokio::sync::Notify::new());
        let first = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let hook = {
            let entered = entered.clone();
            let release = release.clone();
            let first = first.clone();
            Arc::new(move || {
                if first.swap(false, Ordering::SeqCst) {
                    entered.wait();
                    release.wait();
                }
            }) as Arc<dyn Fn() + Send + Sync>
        };
        let owner = Persistence {
            tx: normal_tx,
            replay_tx,
            normal_sequence: Arc::new(AtomicU64::new(0)),
            admission: Arc::new(tokio::sync::Mutex::new(())),
            budgets: AdmissionBudgets::global(),
            admission_hook: Some(hook),
            replay_admission_hook: Some({
                let admitted = admitted.clone();
                Arc::new(move || admitted.notify_one())
            }),
        };
        let running = tokio::spawn(run_owner(normal_rx, replay_rx, state));
        let order = Arc::new(Mutex::new(Vec::new()));

        let first_owner = owner.clone();
        let first_order = order.clone();
        let first_job = tokio::spawn(async move {
            let rx = first_owner
                .submit_resumable_wait(
                    1,
                    async { Ok(()) },
                    move |_, _| {
                        first_order.lock().unwrap().push("normal-1");
                        Ok(Step::Done(()))
                    },
                    |result| async { result },
                )
                .await?;
            receipt(rx).await
        });
        tokio::task::spawn_blocking(move || entered.wait())
            .await
            .unwrap();

        let replay_owner = owner.clone();
        let replay_order = order.clone();
        let capture = tokio::spawn(async move {
            let rx = replay_owner
                .submit_replay_resumable_wait(
                    true,
                    1,
                    async { Ok(()) },
                    move |_, _| {
                        replay_order.lock().unwrap().push("capture");
                        Ok(Step::Done(()))
                    },
                    |result| async { result },
                )
                .await?;
            receipt(rx).await
        });
        tokio::task::spawn_blocking(move || release.wait())
            .await
            .unwrap();
        admitted.notified().await;
        let later_owner = owner.clone();
        let later_order = order.clone();
        let later = tokio::spawn(async move {
            let rx = later_owner
                .submit_resumable_wait(
                    1,
                    async { Ok(()) },
                    move |_, _| {
                        later_order.lock().unwrap().push("normal-2");
                        Ok(Step::Done(()))
                    },
                    |result| async { result },
                )
                .await?;
            receipt(rx).await
        });
        first_job.await.unwrap().unwrap();
        capture.await.unwrap().unwrap();
        later.await.unwrap().unwrap();
        drop(owner);
        running.await.unwrap();
        assert_eq!(*order.lock().unwrap(), ["normal-1", "capture", "normal-2"]);
    }

    #[test]
    fn retained_checkpoint_payload_never_materializes_the_plain_state_document() {
        let meta = crate::store::shell_selection::tests::producer_metadata();
        let row = meta["checkpoint2"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["kind"] == "Checkpoint")
            .unwrap();
        let bytes = crate::store::shell_selection::tests::producer_frame(&meta, row, 2);
        let crate::transport::Envelope::Delta { delta, .. } =
            crate::transport::decode(&bytes[4..]).unwrap()
        else {
            panic!("checkpoint fixture is not a delta")
        };
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let mut state = state.lock().unwrap();
        state.doc = Some(automerge::AutoCommit::new());
        assert!(!state.would_materialize(&delta));
        assert!(!state.materialize(&delta));
    }

    /// A sealed or wrapped delta is not handed to Automerge's parser, which
    /// logs every chunk it rejects; an Automerge change still materializes.
    #[test]
    fn only_automerge_chunks_reach_the_parser() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tracing_subscriber::layer::SubscriberExt as _;

        struct Complaints(Arc<AtomicUsize>);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Complaints {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _: tracing_subscriber::layer::Context<'_, S>,
            ) {
                if *event.metadata().level() <= tracing::Level::WARN {
                    self.0.fetch_add(1, Ordering::SeqCst);
                }
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let mut state = state.lock().unwrap();
        state.doc = Some(automerge::AutoCommit::new());
        let mut author = automerge::AutoCommit::new();
        automerge::transaction::Transactable::put(&mut author, automerge::ROOT, "title", "Q3")
            .unwrap();
        let change = author.save_incremental();
        let complaints = Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry().with(Complaints(Arc::clone(&complaints)));
        tracing::subscriber::with_default(subscriber, || {
            for sealed in [
                &b"\x02sealed channel payload"[..],
                &[0x85, 0x6f][..],
                &[][..],
            ] {
                assert!(!state.would_materialize(sealed));
                assert!(!state.materialize(sealed));
            }
            assert!(state.would_materialize(&change));
            assert!(state.materialize(&change));
        });
        assert_eq!(complaints.load(Ordering::SeqCst), 0);
    }
}
