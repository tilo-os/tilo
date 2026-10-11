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
/// Jobs on the fast path at once, process-wide ([`Lane`]). Each holds a
/// blocking-pool thread for its turn, waits included, so this keeps most of
/// the daemon's pool (`tilo_rt`'s 32 threads) for everything else.
const FAST_TURNS: usize = 2 * BLOCKING_WORKERS;
const RECLAIM_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);
const RECLAIM_RETRY: std::time::Duration = std::time::Duration::from_millis(100);
/// How far past the sequence it hands out [`State::next_sequence`] reserves
/// on disk. A restart continues past the reservation, at most this far
/// ahead of the host clock. Sequences follow the clock, so a workspace that
/// commits steadily renews the reservation once per this interval: at a
/// second, a typing room paid a renewal every second (TIL-732).
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

fn fast_turn_budget() -> &'static Arc<Semaphore> {
    static TURNS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    TURNS.get_or_init(|| Arc::new(Semaphore::new(FAST_TURNS)))
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
    fast_turns: Arc<Semaphore>,
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
            fast_turns: fast_turn_budget().clone(),
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
            fast_turns: Arc::new(Semaphore::new(FAST_TURNS)),
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
    lane: Arc<Lane>,
    fast: FastRunner,
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

#[allow(
    clippy::significant_drop_tightening,
    reason = "the owner state moves into the owner task and the fast runner; no local holds its lock"
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
    let lane = Arc::new(Lane::default());
    let fast = fast_runner(&state, &lane);
    tokio::spawn(run_owner(rx, replay_rx, state, lane.clone()));
    let handle = Persistence {
        tx,
        replay_tx,
        lane,
        fast,
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

#[cfg(test)]
type TurnCounts = Mutex<std::collections::HashMap<std::path::PathBuf, u64>>;

/// Idle owner turns per store root, for tests that bound them.
#[cfg(test)]
fn idle_turns() -> &'static TurnCounts {
    static TURNS: OnceLock<TurnCounts> = OnceLock::new();
    TURNS.get_or_init(Mutex::default)
}

/// Normal jobs the owner task ran itself, per store root.
#[cfg(test)]
fn owner_jobs() -> &'static TurnCounts {
    static JOBS: OnceLock<TurnCounts> = OnceLock::new();
    JOBS.get_or_init(Mutex::default)
}

#[cfg(test)]
fn note_turn(counts: &TurnCounts, state: &Mutex<State>) {
    let root = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .store
        .root()
        .to_path_buf();
    counts
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(root)
        .and_modify(|turns| *turns = turns.saturating_add(1))
        .or_insert(1);
}

/// Whether a room's next job may skip its owner task.
///
/// The owner task runs every job it dequeues, and each one hands off three
/// times: the admitting session wakes the owner, the owner wakes a pool thread
/// to persist, the pool thread wakes the owner to publish. A normal job
/// admitted while the owner has nothing else (nothing queued or running, no
/// replay pending, no entry to reclaim) instead runs, preparation to
/// publication, on the pool thread that persists it, and the owner task does
/// not wake for it: on a quiet room every edit is such a job. Order stays the
/// owner's. The fast job takes the place of an empty queue; every job
/// admitted after it queues, and the owner waits for it to finish, and
/// disposes of what it left, before it touches the state.
#[derive(Default)]
struct Lane {
    /// Jobs admitted and not finished (a fast or handed-off one included) in
    /// the low bits, and the [`Lane::FAST`], [`Lane::HANDOFF`] and
    /// [`Lane::OPEN`] flags: one word, so admission decides atomically.
    word: AtomicU64,
    /// The highest normal job sequence finished, for replay prerequisites.
    completed_normal: AtomicU64,
    /// Notified as a fast job finishes, for an owner waiting to go on.
    done: tokio::sync::Notify,
    /// Notified when a finished fast job left the owner something to do.
    handoff: tokio::sync::Notify,
}

impl Lane {
    /// A job runs on the fast path.
    const FAST: u64 = 1 << 48;
    /// The finished fast job left scratch to dispose of or a replay entry to
    /// reclaim, which the owner does before anything else. It counts as
    /// admitted until then.
    const HANDOFF: u64 = 1 << 49;
    /// The owner serves (its startup reclamation done), reclaims no replay
    /// entry and has not ended, so a job may take the fast path.
    const OPEN: u64 = 1 << 50;
    const ADMITTED: u64 = Self::FAST - 1;

    /// Apply `change` to the word atomically; the word before it.
    fn update(&self, change: impl Fn(u64) -> u64) -> u64 {
        let mut old = self.word.load(Ordering::Acquire);
        loop {
            match self.word.compare_exchange_weak(
                old,
                change(old),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return old,
                Err(current) => old = current,
            }
        }
    }

    fn with_admitted(word: u64, admitted: u64) -> u64 {
        (word & !Self::ADMITTED) | admitted.min(Self::ADMITTED)
    }

    fn may_take(word: u64) -> bool {
        word & Self::OPEN != 0 && word & Self::ADMITTED == 0
    }

    /// Count one job admitted under the admission lock. Whether it takes the
    /// fast path, which it may only when `fast`.
    fn admit(&self, fast: bool) -> bool {
        let old = self.update(|word| {
            let next = Self::with_admitted(word, (word & Self::ADMITTED).saturating_add(1));
            if fast && Self::may_take(word) {
                next | Self::FAST
            } else {
                next
            }
        });
        fast && Self::may_take(old)
    }

    /// `jobs` admitted jobs finished, or never reached the owner.
    fn finished(&self, jobs: u64) {
        self.update(|word| Self::with_admitted(word, (word & Self::ADMITTED).saturating_sub(jobs)));
    }

    /// The fast job finished; `left` says it left the owner work, which keeps
    /// it admitted until the owner has done it.
    fn fast_finished(&self, sequence: u64, left: bool) {
        self.completed_normal.fetch_max(sequence, Ordering::AcqRel);
        self.update(|word| {
            let word = word & !Self::FAST;
            if left {
                word | Self::HANDOFF
            } else {
                Self::with_admitted(word, (word & Self::ADMITTED).saturating_sub(1))
            }
        });
        self.done.notify_one();
        if left {
            self.handoff.notify_one();
        }
    }

    fn completed(&self, sequence: u64) {
        self.completed_normal.fetch_max(sequence, Ordering::AcqRel);
    }

    fn completed_normal(&self) -> u64 {
        self.completed_normal.load(Ordering::Acquire)
    }

    fn set_open(&self, open: bool) {
        self.update(|word| {
            if open {
                word | Self::OPEN
            } else {
                word & !Self::OPEN
            }
        });
    }

    /// Nothing admitted, and a job may take the fast path.
    #[cfg(test)]
    fn quiet(&self) -> bool {
        Self::may_take(self.word.load(Ordering::Acquire))
    }

    /// Wait until no job runs on the fast path. Whether the last one handed
    /// work to the owner, which now does it. Only the owner clears the
    /// hand-off, and no fast job starts while one is pending.
    async fn settled_fast(&self) -> bool {
        loop {
            let done = self.done.notified();
            tokio::pin!(done);
            done.as_mut().enable();
            if self.word.load(Ordering::Acquire) & Self::FAST == 0 {
                return self.word.fetch_and(!Self::HANDOFF, Ordering::AcqRel) & Self::HANDOFF != 0;
            }
            done.await;
        }
    }
}

/// Closes the lane as the owner task ends: a job queued to a dead owner
/// fails, and one on the fast path would outlive it.
struct CloseLane(Arc<Lane>);

impl Drop for CloseLane {
    fn drop(&mut self) {
        self.0.set_open(false);
    }
}

/// Ends a fast job's turn however the job ends, a panic included, and hands
/// the owner anything it left.
struct FastTurn {
    lane: Arc<Lane>,
    state: Arc<Mutex<State>>,
    sequence: u64,
}

impl Drop for FastTurn {
    fn drop(&mut self) {
        let left = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.retention_scratch.is_some()
                || !state.retention_candidates.is_empty()
                || state.replays.reclaimable()
        };
        self.lane.fast_finished(self.sequence, left);
    }
}

/// Runs a job on the fast path: a closure over the owner's state, so that a
/// [`Persistence`] handle does not own the state's lock and holding one is
/// not holding a lock.
type FastRunner =
    Arc<dyn Fn(&tokio::runtime::Handle, OwnedSemaphorePermit, NormalJob) + Send + Sync>;

#[allow(
    clippy::significant_drop_tightening,
    reason = "the state's Arc moves into the runner; no local holds its lock"
)]
fn fast_runner(state: &Arc<Mutex<State>>, lane: &Arc<Lane>) -> FastRunner {
    let state = state.clone();
    let lane = lane.clone();
    Arc::new(move |runtime, fast_turn, job| {
        run_fast(&state, &lane, runtime, fast_turn, job);
    })
}

/// Run `job` on a pool thread for the turn `fast_turn` admits: its blocking
/// quanta run there, each under a worker as on the owner, and its
/// preparation and publication wait there.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the state's Arc and the turn guard move into the pool closure; no local holds a lock"
)]
fn run_fast(
    state: &Arc<Mutex<State>>,
    lane: &Arc<Lane>,
    runtime: &tokio::runtime::Handle,
    fast_turn: OwnedSemaphorePermit,
    job: NormalJob,
) {
    let turn = FastTurn {
        lane: lane.clone(),
        state: state.clone(),
        sequence: job.sequence,
    };
    let state = state.clone();
    let driver = runtime.clone();
    // Detached: the job answers through its receipt, and `turn` hands the
    // lane back as the job ends, a panic included.
    drop(runtime.spawn_blocking(move || {
        let inline = InlineBlocking::enter();
        driver.block_on((job.job)(state));
        drop(inline);
        drop(turn);
        drop(fast_turn);
    }));
}

thread_local! {
    /// Set on a pool thread while it runs a fast job: [`blocking`] work there
    /// runs where it is, under a worker it takes per call as anywhere else.
    static FAST_TURN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct InlineBlocking;

impl InlineBlocking {
    fn enter() -> Self {
        FAST_TURN.with(|fast| fast.set(true));
        Self
    }
}

impl Drop for InlineBlocking {
    fn drop(&mut self) {
        FAST_TURN.with(|fast| fast.set(false));
    }
}

/// Dispose of what the last job left: its scratch root and staged checkpoint
/// generations. One retained transaction can leave at most one known scratch
/// root, so this finishes before another mutation can create one. False when
/// no worker is left, which ends the owner.
async fn settle_retention(state: &Arc<Mutex<State>>, workers: &Arc<Semaphore>) -> bool {
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
                return false;
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
                        discard = crate::store::retained::CheckpointDiscard::new(&lineage, staged)?;
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
                return false;
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
    true
}

#[expect(
    clippy::too_many_lines,
    reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
)]
async fn run_owner(
    mut rx: mpsc::Receiver<NormalJob>,
    mut replay_rx: mpsc::Receiver<ReplayJob>,
    state: Arc<Mutex<State>>,
    lane: Arc<Lane>,
) {
    // However the owner ends, no job takes the fast path past it.
    let _closed = CloseLane(lane.clone());
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
    let mut pending_replay = None;
    lane.set_open(true);
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
        // A normal job admitted before a pending replay may still run on the
        // fast path; the replay's watermark counts it once it has finished.
        // None starts while a replay is pending.
        if pending_replay.is_some() && lane.settled_fast().await {
            if !settle_retention(&state, &workers).await {
                return;
            }
            lane.finished(1);
        }
        // A first shell basis may depend on normal jobs already queued when the
        // capture was admitted. Its fixed watermark excludes later traffic, so
        // unrelated continuous input cannot extend this prerequisite window.
        let mut waiting_prerequisite = pending_replay
            .as_ref()
            .and_then(|job| job.prerequisite)
            .is_some_and(|required| required > lane.completed_normal());
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
                            () = lane.handoff.notified() => Work::Idle,
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
                () = lane.handoff.notified() => Work::Idle,
                _ = idle.tick(), if reclaiming => Work::Idle,
            }
        } else {
            Work::Closed
        };
        // A job on the fast path finishes before this turn touches the
        // state, and what it left to dispose of goes before anything else.
        if !matches!(work, Work::Closed) && lane.settled_fast().await {
            if !settle_retention(&state, &workers).await {
                return;
            }
            lane.finished(1);
        }
        let mut finished = 0;
        match work {
            Work::Normal(job) => {
                #[cfg(test)]
                note_turn(owner_jobs(), &state);
                (job.job)(state.clone()).await;
                lane.completed(job.sequence);
                finished = 1;
                prefer_normal = false;
            }
            Work::Replay(job) => {
                (job.job)(state.clone()).await;
                finished = 1;
                prefer_normal = true;
            }
            Work::QueueReplay(job) => pending_replay = Some(job),
            Work::Idle => {
                #[cfg(test)]
                note_turn(idle_turns(), &state);
            }
            Work::Closed => break,
        }
        if !settle_retention(&state, &workers).await {
            return;
        }
        // An entry still in use needs no turn: its ticket's drop wakes the
        // owner. So does an entry whose session or connection ended, since
        // that session drops its ticket as it ends. While it reclaims, the
        // owner runs every job itself, between steps: the lane closes before
        // this turn's job counts as finished, so no job takes the fast path
        // past a reclamation step.
        let reclaimable = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replays
            .reclaimable();
        if reclaimable && !reclaiming {
            // The first idle turn comes an interval from now, not at once
            // for every interval missed while there was nothing to reclaim.
            idle.reset();
        }
        reclaiming = reclaimable;
        lane.set_open(!reclaiming);
        lane.finished(finished);
        if !reclaiming {
            continue;
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
        self.dispatch(permit, NormalJob { sequence, job });
        Ok(rx)
    }

    /// Hand an admitted normal job, under the admission lock, to the fast
    /// path when its owner is idle and a fast turn is free ([`Lane`]), else
    /// to the owner's queue.
    fn dispatch(&self, permit: mpsc::Permit<'_, NormalJob>, job: NormalJob) {
        let fast = tokio::runtime::Handle::try_current()
            .ok()
            .zip(self.budgets.fast_turns.clone().try_acquire_owned().ok());
        if self.lane.admit(fast.is_some())
            && let Some((runtime, turn)) = fast
        {
            drop(permit);
            (self.fast)(&runtime, turn, job);
        } else {
            permit.send(job);
        }
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
        self.dispatch(permit, NormalJob { sequence, job });
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
        // A bounded input sequence may wait on anything; the owner runs it.
        self.lane.admit(false);
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
        self.lane.admit(false);
        if self
            .replay_tx
            .send(ReplayJob { prerequisite, job })
            .await
            .is_err()
        {
            self.lane.finished(1);
            return Err(error("state persistence unavailable"));
        }
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
    if FAST_TURN.with(std::cell::Cell::get) {
        // A fast job, already on a pool thread. Its quanta take a worker
        // each, so a long one still lets other workspaces in between slices.
        // A panic fails the operation as a panicked pool task would.
        let _permit = workers
            .acquire_owned()
            .await
            .map_err(|e| error(e.to_string()))?;
        return std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation))
            .map_err(|_| error("blocking operation panicked"));
    }
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
        let lane = Arc::new(Lane::default());
        let owner = Persistence {
            tx,
            replay_tx,
            lane: lane.clone(),
            fast: fast_runner(&state, &lane),
            normal_sequence: Arc::new(AtomicU64::new(0)),
            admission: Arc::new(tokio::sync::Mutex::new(())),
            budgets,
            admission_hook: None,
            replay_admission_hook: None,
        };
        let running = tokio::spawn(run_owner(rx, replay_rx, state, lane));
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

    fn turns_of(counts: &TurnCounts, root: &std::path::Path) -> u64 {
        counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(root)
            .copied()
            .unwrap_or(0)
    }

    fn idle_turns_of(root: &std::path::Path) -> u64 {
        turns_of(super::idle_turns(), root)
    }

    fn owner_jobs_of(root: &std::path::Path) -> u64 {
        turns_of(super::owner_jobs(), root)
    }

    /// Until the owner has nothing admitted and lets jobs take the fast path.
    async fn quiesced(owner: &Persistence) {
        tilo_test_env::within("the owner to have nothing admitted", async {
            while !owner.lane.quiet() {
                tokio::task::yield_now().await;
            }
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_idle_owners_jobs_run_without_its_task() {
        let dir = tempfile::tempdir().unwrap();
        let (owner, _running) = test_persistence(
            dir.path(),
            AdmissionBudgets::isolated(isolated_test_workers()),
        );
        let root = store_root(&owner).await;
        let before = owner_jobs_of(&root);
        let order = Arc::new(Mutex::new(Vec::new()));
        for n in 0..8 {
            quiesced(&owner).await;
            let order = order.clone();
            receipt(
                owner
                    .read(1, move |_| {
                        order.lock().unwrap().push(n);
                        Ok(())
                    })
                    .unwrap(),
            )
            .await
            .unwrap();
        }
        assert_eq!(*order.lock().unwrap(), (0..8).collect::<Vec<_>>());
        assert_eq!(
            owner_jobs_of(&root) - before,
            0,
            "a job admitted to an idle owner runs on its pool thread, not the owner task"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_job_admitted_behind_a_fast_job_waits_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let (owner, _running) = test_persistence(
            dir.path(),
            AdmissionBudgets::isolated(isolated_test_workers()),
        );
        let root = store_root(&owner).await;
        quiesced(&owner).await;
        let before = owner_jobs_of(&root);
        let order = Arc::new(Mutex::new(Vec::new()));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let first_order = order.clone();
        let first = owner
            .read(1, move |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                first_order.lock().unwrap().push("fast");
                Ok(())
            })
            .unwrap();
        tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
            .await
            .unwrap();
        let second_order = order.clone();
        let second = owner
            .read(1, move |_| {
                second_order.lock().unwrap().push("queued");
                Ok(())
            })
            .unwrap();
        tokio::time::sleep(RECLAIM_INTERVAL * 5).await;
        assert!(
            order.lock().unwrap().is_empty(),
            "a job ran while the fast job before it held the owner"
        );
        release_tx.send(()).unwrap();
        receipt(first).await.unwrap();
        receipt(second).await.unwrap();
        assert_eq!(*order.lock().unwrap(), ["fast", "queued"]);
        assert_eq!(
            owner_jobs_of(&root) - before,
            1,
            "the job admitted behind the fast one ran on the owner task"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_fast_jobs_scratch_is_disposed_of_before_the_next_job() {
        let dir = tempfile::tempdir().unwrap();
        let (owner, _running) = test_persistence(
            dir.path(),
            AdmissionBudgets::isolated(isolated_test_workers()),
        );
        let root = store_root(&owner).await;
        quiesced(&owner).await;
        let scratch = root
            .join("store-work-v1")
            .join(format!("retain-{}", "0f".repeat(16)));
        let made = scratch.clone();
        receipt(
            owner
                .read(1, move |state| {
                    std::fs::create_dir_all(&made).map_err(StoreError::from)?;
                    state.retention_scratch = Some(made);
                    Ok(())
                })
                .unwrap(),
        )
        .await
        .unwrap();
        let seen = scratch.clone();
        let survived = receipt(owner.read(1, move |_| Ok(seen.exists())).unwrap())
            .await
            .unwrap();
        assert!(
            !survived,
            "the scratch a fast job left was disposed of before the next job ran"
        );
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
            fast_turns: Arc::new(Semaphore::new(FAST_TURNS)),
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
        let lane = Arc::new(Lane::default());
        let owner = Persistence {
            tx: normal_tx,
            replay_tx,
            lane: lane.clone(),
            fast: fast_runner(&state, &lane),
            normal_sequence: Arc::new(AtomicU64::new(0)),
            admission: Arc::new(tokio::sync::Mutex::new(())),
            budgets,
            admission_hook: None,
            replay_admission_hook: None,
        };
        let running = tokio::spawn(run_owner(normal_rx, replay_rx, state, lane));
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
        let running = tokio::spawn(run_owner(normal_rx, replay_rx, state, Arc::default()));
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
        Box::pin(run_owner(normal_rx, replay_rx, state, Arc::default())).await;
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
        Box::pin(run_owner(normal_rx, replay_rx, state, Arc::default())).await;
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
        let lane = Arc::new(Lane::default());
        let owner = Persistence {
            tx: normal_tx,
            replay_tx,
            lane: lane.clone(),
            fast: fast_runner(&state, &lane),
            normal_sequence: Arc::new(AtomicU64::new(0)),
            admission: Arc::new(tokio::sync::Mutex::new(())),
            budgets: AdmissionBudgets::global(),
            admission_hook: Some(hook),
            replay_admission_hook: Some({
                let admitted = admitted.clone();
                Arc::new(move || admitted.notify_one())
            }),
        };
        let running = tokio::spawn(run_owner(normal_rx, replay_rx, state, lane));
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
