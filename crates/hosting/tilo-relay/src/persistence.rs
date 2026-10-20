//! Ordered, bounded state work. The owner awaits publication before dequeuing
//! its successor; callers only wait on receipts and cannot cancel admitted work.
//!
//! Commits this host authors itself (room and channel governance) queue in
//! a lane of their own ([`Lane::Host`]), with their own admission budget,
//! and the owner takes them ahead of the members' queue: a room's governance
//! never waits behind a crowd's traffic. Host order (`seq`) is still the
//! order of owner turns, and so of publication.
//!
//! # What a commit guarantees after a crash
//!
//! An admitted delta is fanned out to other sessions, and a `Commit` is
//! answered with `Committed`, only after its retained record (or pending
//! fallback) and every directory entry leading to it have been fsynced. So
//! an edit any peer has received, or any author was told is committed,
//! survives a crash or power loss in retained history, in order.
//!
//! Commits share their barriers ([`Group`]): the owner runs every commit
//! already queued, each appending its record unsynced, then syncs all their
//! records at once, renames them into place and syncs each generation
//! directory once, and only then completes them, in order. No commit waits
//! for one not yet queued, and none is published before the barrier that
//! covers it. Alone, an edit costs two fsyncs (the record, then its
//! directory); N queued edits cost their N file barriers, issued together,
//! and one directory barrier. Add [`State::next_sequence`]'s reservation
//! renewal at most once per [`SEQUENCE_RESERVATION_NS`] of host clock per
//! workspace. A checkpoint that starts its lineage (each
//! newcomer's first record), or stands alone and replaces the whole of it
//! (a device's next access request or acknowledgement), is staged and made
//! durable before its turn, which then selects the stage with one barrier,
//! provided the lineage has not moved since
//! (`WorkspaceStore::stage_fresh_checkpoint`).
//!
//! A replay capture lists the retained frames in its turn and pins them when
//! it is prepared, off the owner; retention collects nothing a listed frame
//! is in until then (`store::replay::CaptureLeases`).
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
use tilo_lock::MutexRecover as _;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
#[cfg(test)]
mod durability_tests;
mod replay;
pub use replay::output_budget;
pub(crate) use replay::{
    Authority as ReplayAuthority, ReplayAdvance, ReplayHandle, ReplayTicket, WrittenBatch,
};

const QUEUE_MESSAGES: usize = 32;
const GLOBAL_MESSAGES: usize = 128;
const GLOBAL_BYTES: usize = 256 * 1024 * 1024;
const REPLAY_MESSAGES: usize = crate::store::replay::DEFAULT_REPLAY_TICKETS;
const REPLAY_BYTES: usize = 64 * 1024;
/// Host-authored commits (room governance, the channel steward's frames)
/// take their own share of the global totals, so a crowd's member traffic
/// never holds them back: a steward pass waits for its own commits, and
/// every member waiting for a channel key waits for that pass.
const HOST_MESSAGES: usize = 16;
/// Enough for the largest host checkpoint (a compacted record) as charged.
const HOST_BYTES: usize = 64 * 1024 * 1024;
/// Host commits one workspace's owner queues.
const HOST_QUEUE_MESSAGES: usize = 16;
/// Host jobs an owner runs in a row while member jobs wait, so a burst of
/// governance cannot hold a room's members back for long either.
const HOST_BURST: u32 = 8;
pub(crate) const NORMAL_MESSAGES: usize = GLOBAL_MESSAGES - REPLAY_MESSAGES - HOST_MESSAGES;
const NORMAL_BYTES: usize = GLOBAL_BYTES - REPLAY_BYTES - HOST_BYTES;
const BLOCKING_WORKERS: usize = 4;
/// The most blocking workers a host with many cores runs at once.
const MAX_BLOCKING_WORKERS: usize = 16;
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
    /// A commit whose completion waits for its group's barriers
    /// ([`Group`]); any other job runs once every held one was released.
    grouped: bool,
    job: Job,
}
/// A grouped job's completion: given whether the barriers covering its
/// writes held, it acknowledges and publishes, or fails.
type Release =
    Box<dyn FnOnce(Result<(), StoreError>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

/// Most commits one group holds before its barriers are issued.
const MAX_GROUP: usize = 64;

/// Group commit: the commits an owner ran since its last barrier, and the
/// records they appended unsynced. The owner runs every commit already
/// queued, then makes all their records durable at once and only then
/// releases their completions, in order: no commit is acknowledged or fanned
/// out before the barrier covering it, and N queued commits share one round
/// of barriers instead of paying two each.
#[derive(Default)]
pub(crate) struct Group {
    records: crate::store::retained::SharedGroup,
    held: Vec<Release>,
}

impl Group {
    /// The records the running commit's appends join.
    pub(crate) fn records(&self) -> crate::store::retained::SharedGroup {
        self.records.clone()
    }

    /// Make every record the group holds durable now.
    pub(crate) fn flush(&self) -> Result<(), StoreError> {
        self.records.flush()
    }
}

/// [`Group::flush`] as a barrier hook ([`tilo_relay_store::with_barrier_hook`]).
fn flush_hook(
    records: crate::store::retained::SharedGroup,
) -> impl FnMut() -> std::io::Result<()> + 'static {
    move || {
        records.flush().map_err(|error| match error {
            StoreError::Io(error) => error,
            other @ StoreError::Json(_) => std::io::Error::other(other.to_string()),
        })
    }
}

/// Issue the barriers of every record the owner's group holds, then release
/// its held completions in order, each told whether the barriers held.
async fn release_group(state: &Arc<Mutex<State>>, workers: &Arc<Semaphore>) {
    let (held, records) = {
        let mut state = state.lock_recover();
        (std::mem::take(&mut state.group.held), state.group.records())
    };
    if held.is_empty() && !records.pending() {
        return;
    }
    let syncing = crate::timing::start();
    let synced = blocking(workers.clone(), move || records.flush())
        .await
        .and_then(std::convert::identity);
    crate::timing::window("owner.group_sync", syncing);
    if let Err(error) = &synced {
        tracing::warn!(%error, commits = held.len(), "group commit barrier failed");
    }
    for release in held {
        let outcome = match &synced {
            Ok(()) => Ok(()),
            Err(error) => Err(self::error(format!("group commit barrier failed: {error}"))),
        };
        release(outcome).await;
    }
}

/// Which of an owner's ordered queues a job is admitted to. Both take their
/// sequences from one counter, so a capture's prerequisite names a prefix
/// of both; each is FIFO, and the owner prefers the host's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lane {
    /// Members' state and every other ordered job.
    Member,
    /// Commits this host authors itself ([`HOST_MESSAGES`]).
    Host,
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

fn host_message_budget() -> &'static Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(Semaphore::new(HOST_MESSAGES)))
}

fn host_byte_budget() -> &'static Arc<Semaphore> {
    static BYTES: OnceLock<Arc<Semaphore>> = OnceLock::new();
    BYTES.get_or_init(|| Arc::new(Semaphore::new(HOST_BYTES)))
}

fn replay_message_budget() -> &'static Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(Semaphore::new(REPLAY_MESSAGES)))
}

fn replay_byte_budget() -> &'static Arc<Semaphore> {
    static BYTES: OnceLock<Arc<Semaphore>> = OnceLock::new();
    BYTES.get_or_init(|| Arc::new(Semaphore::new(REPLAY_BYTES)))
}

/// Blocking workers every workspace's owner, replay and publication share:
/// one per core the process may use, between [`BLOCKING_WORKERS`] (what a
/// small node keeps) and [`MAX_BLOCKING_WORKERS`]. A crowd joining a room
/// prepares one replay per newcomer, each as large as the room, and four
/// workers left a large host's cores idle while the replays queued.
fn blocking_workers() -> usize {
    std::thread::available_parallelism().map_or(BLOCKING_WORKERS, |cores| {
        cores.get().clamp(BLOCKING_WORKERS, MAX_BLOCKING_WORKERS)
    })
}

/// Detached replay preparation takes at most half the blocking workers, so
/// owners always find one.
fn detached_replay_budget() -> &'static Arc<Semaphore> {
    static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    WORKERS.get_or_init(|| Arc::new(Semaphore::new((blocking_workers() / 2).max(2))))
}

#[derive(Clone)]
struct AdmissionBudgets {
    normal_messages: Arc<Semaphore>,
    normal_bytes: Arc<Semaphore>,
    host_messages: Arc<Semaphore>,
    host_bytes: Arc<Semaphore>,
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
            host_messages: host_message_budget().clone(),
            host_bytes: host_byte_budget().clone(),
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
            host_messages: Arc::new(Semaphore::new(HOST_MESSAGES)),
            host_bytes: Arc::new(Semaphore::new(HOST_BYTES)),
            replay_messages: Arc::new(Semaphore::new(REPLAY_MESSAGES)),
            replay_bytes: Arc::new(Semaphore::new(REPLAY_BYTES)),
            workers,
            replay_worker: Arc::new(Semaphore::new(1)),
        }
    }

    /// The message and byte budgets of `lane`.
    const fn lane(&self, lane: Lane) -> (&Arc<Semaphore>, &Arc<Semaphore>) {
        match lane {
            Lane::Member => (&self.normal_messages, &self.normal_bytes),
            Lane::Host => (&self.host_messages, &self.host_bytes),
        }
    }
}

fn charged_bytes(bytes: usize) -> Result<u32, StoreError> {
    charged_lane_bytes(Lane::Member, bytes)
}

fn charged_lane_bytes(lane: Lane, bytes: usize) -> Result<u32, StoreError> {
    let bytes = bytes.max(1);
    let capacity = match lane {
        Lane::Member => NORMAL_BYTES,
        Lane::Host => HOST_BYTES,
    };
    if bytes > capacity {
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
    host_tx: mpsc::Sender<NormalJob>,
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
    /// The running normal job changes retained history only through
    /// retention, which forgets what captures listed of each lineage it
    /// writes ([`State::retaining_only`]); unless it says so, the owner
    /// forgets every listing after it.
    retaining_only: bool,
    /// The commits and records waiting for one barrier ([`Group`]).
    pub(crate) group: Group,
}
pub(crate) fn error(message: impl Into<String>) -> StoreError {
    StoreError::Io(std::io::Error::other(message.into()))
}

impl State {
    /// The running job changes retained history only through retention
    /// (`RetainBuild`) or a channel's replacement (compaction), each of
    /// which forgets what captures listed of what it writes: what they
    /// listed of the rest stays valid after the job
    /// (`WorkspaceStore::forget_listed`).
    pub(crate) const fn retaining_only(&mut self) {
        self.retaining_only = true;
    }

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
        let budgets = test_budgets().lock_recover().remove(runtime.store.root());
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
    let (host_tx, host_rx) = mpsc::channel::<NormalJob>(HOST_QUEUE_MESSAGES);
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
        retaining_only: false,
        group: Group::default(),
    }));
    tokio::spawn(run_owner_lanes(Lanes::new(rx, host_rx), replay_rx, state));
    let handle = Persistence {
        tx,
        host_tx,
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
            .lock_recover()
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
    let root = state.lock_recover().store.root().to_path_buf();
    idle_turns()
        .lock_recover()
        .entry(root)
        .and_modify(|turns| *turns = turns.saturating_add(1))
        .or_insert(1);
}

/// An owner's two ordered queues ([`Lane`]): the host's first, but never
/// more than [`HOST_BURST`] host jobs in a row while a member job waits.
struct Lanes {
    member: mpsc::Receiver<NormalJob>,
    host: mpsc::Receiver<NormalJob>,
    member_open: bool,
    host_open: bool,
    /// Host jobs taken since the last member job.
    host_run: u32,
}

impl Lanes {
    const fn new(member: mpsc::Receiver<NormalJob>, host: mpsc::Receiver<NormalJob>) -> Self {
        Self {
            member,
            host,
            member_open: true,
            host_open: true,
            host_run: 0,
        }
    }

    const fn open(&self) -> bool {
        self.member_open || self.host_open
    }

    fn take(&mut self, lane: Lane, job: NormalJob) -> NormalJob {
        match lane {
            Lane::Host => self.host_run = self.host_run.saturating_add(1),
            Lane::Member => self.host_run = 0,
        }
        job
    }

    fn try_lane(&mut self, lane: Lane) -> Option<NormalJob> {
        let (rx, open) = match lane {
            Lane::Member => (&mut self.member, &mut self.member_open),
            Lane::Host => (&mut self.host, &mut self.host_open),
        };
        if !*open {
            return None;
        }
        match rx.try_recv() {
            Ok(job) => Some(self.take(lane, job)),
            Err(mpsc::error::TryRecvError::Disconnected) => {
                *open = false;
                None
            }
            Err(mpsc::error::TryRecvError::Empty) => None,
        }
    }

    /// The next queued job, without waiting.
    fn try_next(&mut self) -> Option<NormalJob> {
        let order = if self.host_run < HOST_BURST {
            [Lane::Host, Lane::Member]
        } else {
            [Lane::Member, Lane::Host]
        };
        order.into_iter().find_map(|lane| self.try_lane(lane))
    }

    /// The next job, waiting for one; `None` once both queues are closed
    /// and drained. Cancel-safe: a job is only taken when this returns it.
    async fn next(&mut self) -> Option<NormalJob> {
        if let Some(job) = self.try_next() {
            return Some(job);
        }
        loop {
            if !self.open() {
                return None;
            }
            let (lane, job) = tokio::select! {
                biased;
                job = self.host.recv(), if self.host_open => (Lane::Host, job),
                job = self.member.recv(), if self.member_open => (Lane::Member, job),
            };
            match job {
                Some(job) => return Some(self.take(lane, job)),
                None if lane == Lane::Host => self.host_open = false,
                None => self.member_open = false,
            }
        }
    }
}

/// The highest normal job sequence every one at or below which is done.
/// The lanes complete out of sequence order, so a capture's prerequisite
/// (every normal job admitted before it) is this watermark, never the
/// highest sequence done.
#[derive(Default)]
struct Completed {
    watermark: u64,
    /// Done above a gap, until the gap closes: at most what both queues hold.
    above: std::collections::BTreeSet<u64>,
}

impl Completed {
    fn done(&mut self, sequence: u64) {
        if sequence <= self.watermark {
            return;
        }
        self.above.insert(sequence);
        while self.above.remove(&(self.watermark.saturating_add(1))) {
            self.watermark = self.watermark.saturating_add(1);
        }
    }
}

/// [`run_owner_lanes`] with no host commits, as tests drive an owner.
#[cfg(test)]
async fn run_owner(
    rx: mpsc::Receiver<NormalJob>,
    replay_rx: mpsc::Receiver<ReplayJob>,
    state: Arc<Mutex<State>>,
) {
    let (_, host_rx) = mpsc::channel(1);
    run_owner_lanes(Lanes::new(rx, host_rx), replay_rx, state).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
)]
async fn run_owner_lanes(
    mut lanes: Lanes,
    mut replay_rx: mpsc::Receiver<ReplayJob>,
    state: Arc<Mutex<State>>,
) {
    let workers = state.lock_recover().workers.clone();
    // The startup scan is never reused after a new ticket is admitted.
    let mut startup = None;
    loop {
        let shared = state.clone();
        let result = blocking(workers.clone(), move || {
            let state = shared.lock_recover();
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
    let reclaim_wake = {
        let state = state.lock_recover();
        let wake = state.replays.reclaim_wake();
        // So are collections that waited for the last capture to pin.
        state.store.wake_on_capture_release(wake.clone());
        wake
    };
    let mut idle = tokio::time::interval(RECLAIM_INTERVAL);
    idle.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut reclaiming = false;
    let mut prefer_normal = false;
    let mut replay_open = true;
    let mut completed = Completed::default();
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
        if !lanes.open() && !replay_open && pending_replay.is_none() {
            break;
        }
        // A first shell basis may depend on normal jobs already queued when the
        // capture was admitted. Its fixed watermark excludes later traffic, so
        // unrelated continuous input cannot extend this prerequisite window.
        let mut waiting_prerequisite = pending_replay
            .as_ref()
            .and_then(|job| job.prerequisite)
            .is_some_and(|required| required > completed.watermark);
        if waiting_prerequisite {
            let shared = state.clone();
            let has_basis = blocking(workers.clone(), move || {
                shared.lock_recover().store.replay_capture_has_basis()
            })
            .await;
            if matches!(has_basis, Ok(Ok(true))) {
                if let Some(job) = pending_replay.as_mut() {
                    job.prerequisite = None;
                }
                waiting_prerequisite = false;
            }
        }
        let work = if waiting_prerequisite && lanes.open() {
            lanes.next().await.map_or(Work::Idle, Work::Normal)
        } else if prefer_normal && lanes.open() {
            if let Some(job) = lanes.try_next() {
                Work::Normal(job)
            } else if let Some(job) = pending_replay.take() {
                Work::Replay(job)
            } else if !lanes.open() && !replay_open {
                // Both lanes closed as they were looked at, and nothing else
                // can wake the owner for work: drain and end.
                Work::Closed
            } else {
                tokio::select! {
                    biased;
                    job = lanes.next(), if lanes.open() => job.map_or(Work::Idle, Work::Normal),
                    job = replay_rx.recv(), if replay_open => if let Some(job) = job { Work::QueueReplay(job) } else { replay_open = false; Work::Idle },
                    () = reclaim_wake.notified(), if !reclaiming => Work::Idle,
                    _ = idle.tick(), if reclaiming => Work::Idle,
                }
            }
        } else if let Some(job) = pending_replay.take() {
            Work::Replay(job)
        } else if lanes.open() || replay_open {
            tokio::select! {
                biased;
                job = replay_rx.recv(), if replay_open => if let Some(job) = job { Work::QueueReplay(job) } else { replay_open = false; Work::Idle },
                job = lanes.next(), if lanes.open() => job.map_or(Work::Idle, Work::Normal),
                () = reclaim_wake.notified(), if !reclaiming => Work::Idle,
                _ = idle.tick(), if reclaiming => Work::Idle,
            }
        } else {
            Work::Closed
        };
        match work {
            Work::Normal(job) => {
                // Every commit already queued runs before the barriers of
                // what they wrote, which come before any of them completes
                // ([`Group`]); the group ends at a job that is no commit.
                let mut grouped = Vec::new();
                let mut next = Some(job);
                while let Some(job) = next.take() {
                    if !job.grouped {
                        release_group(&state, &workers).await;
                        for sequence in std::mem::take(&mut grouped) {
                            completed.done(sequence);
                        }
                    }
                    state.lock_recover().retaining_only = false;
                    (job.job)(state.clone()).await;
                    let state = state.lock_recover();
                    // What captures listed stays valid after a job that changed
                    // retained history only through retention; any other may
                    // have changed any lineage.
                    if !state.retaining_only {
                        // Counted: the jobs after which every listing is gone.
                        crate::timing::window("owner.listing_forgotten", crate::timing::start());
                        state.store.forget_listed(None);
                    }
                    if !job.grouped {
                        completed.done(job.sequence);
                        break;
                    }
                    grouped.push(job.sequence);
                    // A job that left scratch or a stage to dispose of ends
                    // its group: its disposal runs before the next mutation.
                    let disposal =
                        state.retention_scratch.is_some() || !state.retention_candidates.is_empty();
                    drop(state);
                    if !disposal && grouped.len() < MAX_GROUP {
                        next = lanes.try_next();
                    }
                }
                release_group(&state, &workers).await;
                for sequence in grouped {
                    completed.done(sequence);
                }
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
        let root = state.lock_recover().retention_scratch.take();
        let disposing = crate::timing::start();
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
        crate::timing::window("owner.scratch_disposal", disposing);
        let disposing = crate::timing::start();
        let candidates = std::mem::take(&mut state.lock_recover().retention_candidates);
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
        // Generations retention replaced while a capture still had to pin
        // them, collected once no capture does. A failed collection is
        // left to the lineage's next commit, which collects all of them.
        crate::timing::window("owner.stage_disposal", disposing);
        let collecting = crate::timing::start();
        let deferred = state.lock_recover().store.deferred_collections();
        for lineage in deferred {
            let mut collect = None;
            loop {
                let lineage = lineage.clone();
                let Ok((next, result)) = blocking(workers.clone(), move || {
                    let result = (|| {
                        let gc = match &mut collect {
                            Some(gc) => gc,
                            None => {
                                collect.insert(crate::store::retained::GenerationGc::new(lineage)?)
                            }
                        };
                        gc.step(WorkLimit::default())
                    })();
                    (collect, result)
                })
                .await
                else {
                    tracing::error!("deferred retention collection worker unavailable");
                    return;
                };
                collect = next;
                match result {
                    Ok(Step::Done(())) => break,
                    Ok(Step::More) => tokio::task::yield_now().await,
                    Err(error) => {
                        tracing::warn!(%error, "deferred retention collection failed");
                        break;
                    }
                }
            }
        }
        crate::timing::window("owner.deferred_collection", collecting);
        // An entry still in use needs no turn: its ticket's drop wakes the
        // owner. So does an entry whose session or connection ended, since
        // that session drops its ticket as it ends.
        if !state.lock_recover().replays.reclaimable() {
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
        let reclaiming_at = crate::timing::start();
        let _ = blocking(workers.clone(), move || {
            let mut state = shared.lock_recover();
            // A released entry keeps its process-wide replay permit until it
            // is gone, so new subscriptions wait on this. One bounded step
            // per turn fell behind a room where every guest's arrival
            // replays to them: spools grow with the room, permits ran out,
            // and joins queued for seconds. Step for one slice instead.
            let start = std::time::Instant::now();
            loop {
                if let Err(error) = state.reclaim_replay_step() {
                    tracing::warn!(%error, "replay reclamation failed");
                    break;
                }
                if start.elapsed() >= SLICE || !state.replays.reclaimable() {
                    break;
                }
            }
            drop(state);
        })
        .await;
        crate::timing::window("owner.reclaim", reclaiming_at);
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
        let (job, rx) = resumable_job(
            slot,
            bytes,
            byte_count,
            NORMAL_STEPS,
            false,
            preparation,
            operation,
            completion,
        );
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
        permit.send(NormalJob {
            sequence,
            grouped: false,
            job,
        });
        Ok(rx)
    }

    /// Durable state and replay apply capacity-driven backpressure instead of
    /// turning a healthy replay burst into a connection error.
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
        self.submit_lane_wait(Lane::Member, bytes, preparation, operation, completion)
            .await
    }

    /// [`Self::submit_resumable_wait`] on `lane`: a host commit waits only
    /// for the host's budgets and queue, and its owner takes it ahead of
    /// the members' ([`Lanes`]).
    pub(crate) async fn submit_lane_wait<P, PF, T, F, C, CF>(
        &self,
        lane: Lane,
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
        self.submit_lane(lane, false, bytes, preparation, operation, completion)
            .await
    }

    /// [`Self::submit_lane_wait`] for a commit that joins its owner's group
    /// ([`Group`]): its completion runs once the barriers covering every
    /// record the operation appended to the group held, and fails if they
    /// did not.
    pub(crate) async fn submit_commit_wait<P, PF, T, F, C, CF>(
        &self,
        lane: Lane,
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
        self.submit_lane(lane, true, bytes, preparation, operation, completion)
            .await
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "the admission permits move into the job; the lint does not see the move"
    )]
    async fn submit_lane<P, PF, T, F, C, CF>(
        &self,
        lane: Lane,
        grouped: bool,
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
        let byte_count = charged_lane_bytes(lane, bytes)?;
        let (messages, byte_budget) = self.budgets.lane(lane);
        let slot = messages
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| error("state persistence unavailable"))?;
        let bytes = byte_budget
            .clone()
            .acquire_many_owned(byte_count)
            .await
            .map_err(|_| error("state persistence unavailable"))?;
        let (job, rx) = resumable_job(
            slot,
            bytes,
            byte_count,
            match lane {
                Lane::Member => NORMAL_STEPS,
                Lane::Host => HOST_STEPS,
            },
            grouped,
            preparation,
            operation,
            completion,
        );
        let queue = match lane {
            Lane::Member => &self.tx,
            Lane::Host => &self.host_tx,
        };
        let permit = queue
            .reserve()
            .await
            .map_err(|_| error("state persistence unavailable"))?;
        let _admission = self.admission.lock().await;
        let sequence = next_normal_job(&self.normal_sequence)?;
        #[cfg(test)]
        if let Some(hook) = &self.admission_hook {
            hook();
        }
        permit.send(NormalJob {
            sequence,
            grouped,
            job,
        });
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
                    let workers = state.lock_recover().workers.clone();
                    let running = crate::timing::start();
                    let result = run(state, workers).await;
                    crate::timing::window("owner.sequence", running);
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
        permit.send(NormalJob {
            sequence,
            grouped: false,
            job,
        });
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
        let (job, rx) = resumable_job(
            slot,
            bytes,
            byte_count,
            REPLAY_STEPS,
            false,
            preparation,
            operation,
            completion,
        );
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
        // Counted: how many of the owner's normal jobs are reads.
        crate::timing::window("owner.read_jobs", crate::timing::start());
        self.submit(bytes, operation, |result| async { result })
    }
}

/// What an owner job's phases are timed as ([`crate::timing::window`]).
#[derive(Clone, Copy)]
struct JobSteps {
    /// From admission to the owner taking it up.
    queue: &'static str,
    /// Its asynchronous preparation, in owner order.
    prepare: &'static str,
    /// Its blocking transaction: every quantum and the waits for a worker.
    persist: &'static str,
    /// Its asynchronous completion (publication), in owner order.
    complete: &'static str,
}
const NORMAL_STEPS: JobSteps = JobSteps {
    queue: "owner.normal.queue",
    prepare: "owner.normal.prepare",
    persist: "owner.normal.persist",
    complete: "owner.normal.complete",
};
const HOST_STEPS: JobSteps = JobSteps {
    queue: "owner.host.queue",
    prepare: "owner.host.prepare",
    persist: "owner.host.persist",
    complete: "owner.host.complete",
};
const REPLAY_STEPS: JobSteps = JobSteps {
    queue: "owner.replay.queue",
    prepare: "owner.replay.prepare",
    persist: "owner.replay.persist",
    complete: "owner.replay.complete",
};

fn resumable_job<P, PF, T, F, C, CF>(
    slot: OwnedSemaphorePermit,
    bytes: OwnedSemaphorePermit,
    byte_count: u32,
    steps: JobSteps,
    grouped: bool,
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
    let timed = crate::timing::start();
    // The operation and its preparation go into and out of the job's box and
    // the blocking pool on every quantum: on the heap, each move is a pointer.
    let mut operation = Box::new(operation);
    let preparation = Box::pin(preparation);
    let job = Box::new(
        move |state: Arc<Mutex<State>>| -> Pin<Box<dyn Future<Output = ()> + Send>> {
            Box::pin(async move {
                let permits = (slot, bytes);
                let workers = state.lock_recover().workers.clone();
                crate::timing::window(steps.queue, timed);
                let preparing = crate::timing::start();
                let result = match preparation.await {
                    Err(error) => Err(error),
                    Ok(prepared) => {
                        crate::timing::window(steps.prepare, preparing);
                        let persisting = crate::timing::start();
                        let mut prepared = Some(prepared);
                        let result = loop {
                            let shared = state.clone();
                            let quantum = blocking(workers.clone(), move || {
                                let start = std::time::Instant::now();
                                let mut state = shared.lock_recover();
                                // Whatever this quantum makes durable is
                                // ordered after every record its group holds.
                                let hook = flush_hook(state.group.records());
                                let result = tilo_relay_store::with_barrier_hook(hook, || {
                                    loop {
                                        let result = operation(&mut state, &mut prepared);
                                        if !matches!(result, Ok(Step::More))
                                            || start.elapsed() >= SLICE
                                        {
                                            break result;
                                        }
                                    }
                                });
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
                        };
                        crate::timing::window(steps.persist, persisting);
                        result
                    }
                };
                // The owner serializes this asynchronous authorization/publication
                // step as well as the blocking transaction, even if tx is closed.
                let complete = move |result| async move {
                    let completing = crate::timing::start();
                    let result = completion(result).await;
                    crate::timing::window(steps.complete, completing);
                    let _ = tx.send(result);
                };
                if !grouped {
                    complete(result).await;
                    drop(permits);
                    return;
                }
                // Completed once the group's barriers covered what it wrote,
                // still holding its admission permits.
                let release: Release = Box::new(move |synced: Result<(), StoreError>| {
                    Box::pin(async move {
                        complete(result.and_then(|value| synced.map(|()| value))).await;
                        drop(permits);
                    })
                });
                state.lock_recover().group.held.push(release);
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
    WORKERS.get_or_init(|| Arc::new(Semaphore::new(blocking_workers())))
}

pub(crate) async fn blocking<T: Send + 'static>(
    workers: Arc<Semaphore>,
    operation: impl FnOnce() -> T + Send + 'static,
) -> Result<T, StoreError> {
    let waiting = crate::timing::start();
    let permit = workers
        .acquire_owned()
        .await
        .map_err(|e| error(e.to_string()))?;
    crate::timing::window("blocking.worker_wait", waiting);
    let dispatched = crate::timing::start();
    tokio::task::spawn_blocking(move || {
        crate::timing::window("blocking.dispatch", dispatched);
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
    use tilo_relay_shell::ShellAuthority as _;

    fn test_persistence(
        root: &std::path::Path,
        budgets: AdmissionBudgets,
    ) -> (Persistence, tokio::task::JoinHandle<()>) {
        let state = state(root);
        let (tx, rx) = mpsc::channel::<NormalJob>(8);
        let (replay_tx, replay_rx) = mpsc::channel::<ReplayJob>(8);
        let owner = Persistence {
            tx,
            host_tx: mpsc::channel(1).0,
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
            retaining_only: false,
            group: Group::default(),
        }))
    }

    fn record_job(order: Arc<Mutex<Vec<&'static str>>>, value: &'static str) -> Job {
        Box::new(move |_| {
            Box::pin(async move {
                order.lock_recover().push(value);
            })
        })
    }

    fn idle_turns_of(root: &std::path::Path) -> u64 {
        super::idle_turns()
            .lock_recover()
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
        let ticket = ReplayTicket::reserve([2; 32], None).await.unwrap();
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
                let owner_workers = state.lock_recover().workers.clone();
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
        assert_eq!(
            NORMAL_MESSAGES + HOST_MESSAGES + REPLAY_MESSAGES,
            GLOBAL_MESSAGES
        );
        assert_eq!(NORMAL_BYTES + HOST_BYTES + REPLAY_BYTES, GLOBAL_BYTES);
        let budgets = AdmissionBudgets::isolated(isolated_test_workers());
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
            host_tx: mpsc::channel(1).0,
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
                grouped: false,
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
                grouped: false,
                job: record_job(order.clone(), "normal-1"),
            })
            .await
            .unwrap();
        normal_tx
            .send(NormalJob {
                sequence: 3,
                grouped: false,
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
            *order.lock_recover(),
            ["replay-1", "normal-1", "replay-2", "normal-2"]
        );
    }

    #[tokio::test]
    async fn first_basis_prerequisites_run_before_priority_capture() {
        let tmp = tempfile::tempdir().unwrap();
        let authority = tilo_relay_shell::selection::fixtures::producer_authority(
            &tilo_relay_shell::selection::fixtures::producer_metadata(),
        );
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
            retaining_only: false,
            group: Group::default(),
        }));
        let (normal_tx, normal_rx) = mpsc::channel::<NormalJob>(8);
        let (replay_tx, replay_rx) = mpsc::channel::<ReplayJob>(8);
        let order = Arc::new(Mutex::new(Vec::new()));
        let normal_order = order.clone();
        normal_tx
            .send(NormalJob {
                sequence: 1,
                grouped: false,
                job: Box::new(move |state| {
                    Box::pin(async move {
                        let root = state
                            .lock_recover()
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

    /// An owner whose store is bound to a shell authority but holds no
    /// basis yet, so a first capture waits for its prerequisite.
    fn unbased_state(root: &std::path::Path) -> Arc<Mutex<State>> {
        let authority = tilo_relay_shell::selection::fixtures::producer_authority(
            &tilo_relay_shell::selection::fixtures::producer_metadata(),
        );
        let workspace = crate::room_workspace_id(authority.room()).unwrap();
        let mut store = WorkspaceStore::open(root, workspace, false).unwrap();
        store.bind_shell_authority(authority).unwrap();
        Arc::new(Mutex::new(State {
            store,
            doc: None,
            sequence: None,
            reserved: 0,
            replays: replay::Replays::default(),
            retention_scratch: None,
            retention_candidates: Vec::new(),
            workers: isolated_test_workers(),
            replay_worker: Arc::new(Semaphore::new(1)),
            retaining_only: false,
            group: Group::default(),
        }))
    }

    /// Host commits go ahead of the member jobs queued before them, at most
    /// [`HOST_BURST`] in a row while a member job waits; each lane keeps
    /// its own order.
    #[tokio::test]
    async fn host_jobs_overtake_queued_member_jobs_a_burst_at_a_time() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let (normal_tx, normal_rx) = mpsc::channel::<NormalJob>(8);
        let (host_tx, host_rx) = mpsc::channel::<NormalJob>(16);
        let (replay_tx, replay_rx) = mpsc::channel::<ReplayJob>(8);
        let order = Arc::new(Mutex::new(Vec::new()));
        let host_jobs = HOST_BURST + 1;
        let labels = (0..host_jobs)
            .map(|index| &*format!("host-{index}").leak())
            .collect::<Vec<&'static str>>();
        for (sequence, label) in [(1, "member-1"), (2, "member-2")] {
            normal_tx
                .send(NormalJob {
                    sequence,
                    grouped: false,
                    job: record_job(order.clone(), label),
                })
                .await
                .unwrap();
        }
        for (index, label) in labels.iter().enumerate() {
            host_tx
                .send(NormalJob {
                    sequence: 3 + index as u64,
                    grouped: false,
                    job: record_job(order.clone(), label),
                })
                .await
                .unwrap();
        }
        drop((normal_tx, host_tx, replay_tx));
        Box::pin(run_owner_lanes(
            Lanes::new(normal_rx, host_rx),
            replay_rx,
            state,
        ))
        .await;
        let burst = usize::try_from(HOST_BURST).unwrap();
        let mut expected = labels[..burst].to_vec();
        expected.extend(["member-1", labels[burst], "member-2"]);
        assert_eq!(*order.lock().unwrap(), expected);
    }

    /// A capture's prerequisite is every normal job admitted before it, on
    /// either lane: a later host commit that completes first does not let
    /// the capture past a member job still queued below it.
    #[tokio::test]
    async fn a_capture_prerequisite_waits_for_every_earlier_job_of_either_lane() {
        let tmp = tempfile::tempdir().unwrap();
        let state = unbased_state(tmp.path());
        let (normal_tx, normal_rx) = mpsc::channel::<NormalJob>(8);
        let (host_tx, host_rx) = mpsc::channel::<NormalJob>(8);
        let (replay_tx, replay_rx) = mpsc::channel::<ReplayJob>(8);
        let order = Arc::new(Mutex::new(Vec::new()));
        normal_tx
            .send(NormalJob {
                sequence: 1,
                grouped: false,
                job: record_job(order.clone(), "member-1"),
            })
            .await
            .unwrap();
        host_tx
            .send(NormalJob {
                sequence: 2,
                grouped: false,
                job: record_job(order.clone(), "host-2"),
            })
            .await
            .unwrap();
        replay_tx
            .send(ReplayJob {
                prerequisite: Some(2),
                job: record_job(order.clone(), "capture"),
            })
            .await
            .unwrap();
        host_tx
            .send(NormalJob {
                sequence: 3,
                grouped: false,
                job: record_job(order.clone(), "host-3"),
            })
            .await
            .unwrap();
        drop((normal_tx, host_tx, replay_tx));
        Box::pin(run_owner_lanes(
            Lanes::new(normal_rx, host_rx),
            replay_rx,
            state,
        ))
        .await;
        assert_eq!(
            *order.lock().unwrap(),
            ["host-2", "host-3", "member-1", "capture"]
        );
    }

    /// What captures listed survives a job that changes retained history
    /// only through retention, and no other job.
    #[tokio::test]
    async fn an_owner_forgets_what_captures_listed_after_a_job_it_cannot_vouch_for() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let store = state.lock().unwrap().store.clone();
        let frame = crate::transport::encode(&crate::transport::Envelope::Delta {
            workspace_id: 7,
            author_device_public_key: [1; 32],
            authored_at_ns: 1,
            signature: [0; 64],
            delta: crate::coedit::CoeditPayload::RetainedDelta {
                channel: [9; 32],
                body: vec![1; 10],
            }
            .encode()
            .unwrap(),
        })
        .unwrap();
        store.retain_delta(&[9; 32], 1, &frame).unwrap();
        let list = || {
            let mut build = store
                .start_replay_capture(crate::store::replay::ReplayId([4; 16]), [2; 32])
                .unwrap();
            let snapshot = loop {
                if let Step::Done(snapshot) = store
                    .capture_replay_step(&mut build, WorkLimit::default())
                    .unwrap()
                {
                    break snapshot;
                }
            };
            let mut discard = snapshot.into_discard().unwrap();
            while discard.step(WorkLimit::default()).unwrap() == Step::More {}
        };
        list();
        assert_eq!(store.listed_lineage_names().len(), 1);
        let (normal_tx, normal_rx) = mpsc::channel::<NormalJob>(8);
        let (replay_tx, replay_rx) = mpsc::channel::<ReplayJob>(8);
        normal_tx
            .send(NormalJob {
                sequence: 1,
                grouped: false,
                job: Box::new(|state| {
                    Box::pin(async move {
                        state.lock().unwrap().retaining_only();
                    })
                }),
            })
            .await
            .unwrap();
        let (vouched_tx, vouched) = oneshot::channel();
        let observed = store.clone();
        normal_tx
            .send(NormalJob {
                sequence: 2,
                grouped: false,
                job: Box::new(move |_| {
                    Box::pin(async move {
                        let _ = vouched_tx.send(observed.listed_lineage_names().len());
                    })
                }),
            })
            .await
            .unwrap();
        drop((normal_tx, replay_tx));
        Box::pin(run_owner(normal_rx, replay_rx, state)).await;
        assert_eq!(
            vouched.await.unwrap(),
            1,
            "a job changing history only through retention keeps the listing"
        );
        assert!(
            store.listed_lineage_names().is_empty(),
            "a job that says nothing of what it changed forgets every listing"
        );
    }

    #[test]
    fn the_completed_watermark_only_moves_over_a_contiguous_prefix() {
        let mut completed = Completed::default();
        completed.done(2);
        completed.done(4);
        assert_eq!(completed.watermark, 0, "sequence 1 is not done");
        completed.done(1);
        assert_eq!(completed.watermark, 2);
        completed.done(3);
        assert_eq!(completed.watermark, 4);
        assert!(completed.above.is_empty());
        completed.done(4);
        assert_eq!(
            completed.watermark, 4,
            "a sequence done twice changes nothing"
        );
    }

    /// A host commit is admitted while the members' normal budget is
    /// exhausted, and runs before the member jobs queued ahead of it.
    #[tokio::test]
    async fn host_admission_has_its_own_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let budgets = AdmissionBudgets::isolated(isolated_test_workers());
        let _members_full = budgets
            .normal_messages
            .clone()
            .acquire_many_owned(u32::try_from(NORMAL_MESSAGES).unwrap())
            .await
            .unwrap();
        let state = state(tmp.path());
        let (normal_tx, normal_rx) = mpsc::channel::<NormalJob>(8);
        let (host_tx, host_rx) = mpsc::channel::<NormalJob>(8);
        let (replay_tx, replay_rx) = mpsc::channel::<ReplayJob>(8);
        let owner = Persistence {
            tx: normal_tx,
            host_tx,
            replay_tx,
            normal_sequence: Arc::new(AtomicU64::new(0)),
            admission: Arc::new(tokio::sync::Mutex::new(())),
            budgets,
            admission_hook: None,
            replay_admission_hook: None,
        };
        let running = tokio::spawn(run_owner_lanes(
            Lanes::new(normal_rx, host_rx),
            replay_rx,
            state,
        ));
        assert!(
            owner
                .submit_resumable(
                    1,
                    async { Ok(()) },
                    |_, _| Ok(Step::Done(())),
                    |result| async { result },
                )
                .is_err(),
            "the members' budget is exhausted"
        );
        let host = tilo_test_env::within(
            "a host commit is admitted past the members' exhausted budget",
            owner.submit_lane_wait(
                Lane::Host,
                1,
                async { Ok(()) },
                |_, _| Ok(Step::Done(7)),
                |result| async { result },
            ),
        )
        .await
        .unwrap();
        assert_eq!(receipt(host).await.unwrap(), 7);
        drop(owner);
        running.await.unwrap();
    }

    #[tokio::test]
    async fn first_basis_capture_waits_only_for_its_fixed_normal_watermark() {
        let tmp = tempfile::tempdir().unwrap();
        let authority = tilo_relay_shell::selection::fixtures::producer_authority(
            &tilo_relay_shell::selection::fixtures::producer_metadata(),
        );
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
            retaining_only: false,
            group: Group::default(),
        }));
        let (normal_tx, normal_rx) = mpsc::channel::<NormalJob>(8);
        let (replay_tx, replay_rx) = mpsc::channel::<ReplayJob>(8);
        let order = Arc::new(Mutex::new(Vec::new()));
        normal_tx
            .send(NormalJob {
                sequence: 1,
                grouped: false,
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
                    grouped: false,
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
        let authority = tilo_relay_shell::selection::fixtures::producer_authority(
            &tilo_relay_shell::selection::fixtures::producer_metadata(),
        );
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
            retaining_only: false,
            group: Group::default(),
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
            host_tx: mpsc::channel(1).0,
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
        let meta = tilo_relay_shell::selection::fixtures::producer_metadata();
        let row = meta["checkpoint2"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["kind"] == "Checkpoint")
            .unwrap();
        let bytes = tilo_relay_shell::selection::fixtures::producer_frame(&meta, row, 2);
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
