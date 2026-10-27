//! Session authority belongs to the ordered owner, never to a persisted manifest.
use super::{State, error};
use crate::store::bounded::Invariant;
use crate::store::{
    StoreError, WorkspaceStore,
    replay::{
        DEFAULT_OUTPUT_BYTES, DEFAULT_REPLAY_TICKETS, ReplayBatch, ReplayBudget, ReplayBuild,
        ReplayId, ReplayLimits, ReplaySnapshot, ReplaySummary, Step, WorkDiscard, WorkLimit,
    },
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
use tilo_lock::MutexRecover as _;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) struct ReplayTicket {
    pub(crate) authority: Arc<Authority>,
}
impl Drop for ReplayTicket {
    fn drop(&mut self) {
        if self.authority.reserved.is_some() {
            let _ = self.authority.released.set(std::time::Instant::now());
        }
        self.authority.cancelled.store(true, Ordering::SeqCst);
        // The owner holding this replay's entry can reclaim it now.
        if let Some(reclaim) = self.authority.reclaim.get() {
            reclaim.notify_one();
        }
    }
}
pub(crate) struct Authority {
    id: ReplayId,
    device: [u8; 32],
    cancelled: AtomicBool,
    connection: Option<Arc<AtomicBool>>,
    session: Mutex<Option<Arc<AtomicBool>>>,
    /// The wake of the owner whose table took this replay's entry
    /// ([`Replays::reclaim_wake`]), set when it took it.
    reclaim: OnceLock<Arc<tokio::sync::Notify>>,
    /// When the slot was taken and when its session let go of it, for the
    /// capacity timings ([`crate::timing`]).
    reserved: Option<std::time::Instant>,
    released: OnceLock<std::time::Instant>,
    /// The replay includes on-demand records: the subscriber is a peer
    /// relay, which keeps all of the room.
    every_record: bool,
    // The owner keeps admission during abandoned-spool reclamation.
    _permit: OwnedSemaphorePermit,
}
impl Drop for Authority {
    fn drop(&mut self) {
        // The slot is free again only now, once the owner reclaimed the
        // entry that held this authority as well.
        crate::timing::event("join.ticket_held", self.reserved, 0);
        crate::timing::event("join.reclaim_lag", self.released.get().copied(), 0);
    }
}
impl Authority {
    /// The bound session flag. A poisoned lock still holds a coherent
    /// `Option`, so recover it instead of propagating another thread's panic.
    fn session_guard(&self) -> std::sync::MutexGuard<'_, Option<Arc<AtomicBool>>> {
        self.session.lock_recover()
    }
    fn active(&self) -> bool {
        !self.cancelled.load(Ordering::SeqCst)
            && self
                .connection
                .as_ref()
                .is_none_or(|live| live.load(Ordering::SeqCst))
            && self
                .session_guard()
                .as_ref()
                .is_none_or(|live| live.load(Ordering::SeqCst))
    }
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the session guard makes the bound check and the bind one atomic step"
    )]
    pub(crate) fn bind(&self, live: Arc<AtomicBool>) -> Result<(), StoreError> {
        let mut session = self.session_guard();
        if session.is_some() || !self.active_unbound() {
            return Err(error("replay binding cancelled"));
        }
        *session = Some(live);
        Ok(())
    }
    fn active_unbound(&self) -> bool {
        !self.cancelled.load(Ordering::SeqCst)
            && self
                .connection
                .as_ref()
                .is_none_or(|live| live.load(Ordering::SeqCst))
    }
    pub(crate) fn matches(&self, live: &Arc<AtomicBool>) -> bool {
        self.active()
            && self
                .session_guard()
                .as_ref()
                .is_some_and(|bound| Arc::ptr_eq(bound, live))
    }
    pub(crate) fn bound_active(&self) -> bool {
        self.active() && self.session_guard().is_some()
    }
}
/// How long a subscription waits for one of the host's replay slots before
/// its session gives up. A join burst queues behind the replays in flight
/// instead of ending every session past the slot count.
const TICKET_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

impl ReplayTicket {
    /// A replay slot for `device`, waiting up to [`TICKET_WAIT`] for one to
    /// free up.
    pub(crate) async fn reserve(
        device: [u8; 32],
        connection: Option<Arc<AtomicBool>>,
    ) -> Result<Self, StoreError> {
        static TICKETS: OnceLock<Arc<Semaphore>> = OnceLock::new();
        let slots = TICKETS
            .get_or_init(|| Arc::new(Semaphore::new(DEFAULT_REPLAY_TICKETS)))
            .clone();
        let permit = tokio::time::timeout(TICKET_WAIT, slots.acquire_owned())
            .await
            .map_err(|_| error("replay ticket backpressure"))?
            .map_err(|_| error("replay tickets closed"))?;
        Ok(Self::with_permit(device, connection, permit))
    }
    #[cfg(test)]
    fn reserve_from(
        device: [u8; 32],
        connection: Option<Arc<AtomicBool>>,
        slots: Arc<Semaphore>,
    ) -> Result<Self, StoreError> {
        let permit = slots
            .try_acquire_owned()
            .map_err(|_| error("replay ticket backpressure"))?;
        Ok(Self::with_permit(device, connection, permit))
    }
    /// Replay on-demand records too: the subscriber keeps all of the room.
    pub(crate) fn with_every_record(mut self) -> Self {
        if let Some(authority) = Arc::get_mut(&mut self.authority) {
            authority.every_record = true;
        }
        self
    }
    fn with_permit(
        device: [u8; 32],
        connection: Option<Arc<AtomicBool>>,
        permit: OwnedSemaphorePermit,
    ) -> Self {
        Self {
            authority: Arc::new(Authority {
                id: ReplayId(*uuid::Uuid::new_v4().as_bytes()),
                device,
                cancelled: AtomicBool::new(false),
                connection,
                session: Mutex::new(None),
                reclaim: OnceLock::new(),
                reserved: crate::timing::start(),
                released: OnceLock::new(),
                every_record: false,
                _permit: permit,
            }),
        }
    }
}
// Only the sole writer constructs this after dropping a fully written batch.
pub(crate) struct WrittenBatch(pub(crate) u64);
pub(crate) enum ReplayAdvance {
    Preparing,
    Batch(ReplayBatch),
    Exhausted(ReplaySummary),
}
#[derive(Clone)]
pub(crate) struct ReplayHandle {
    detached: Arc<Mutex<Option<DetachedReplay>>>,
    authority: Weak<Authority>,
    workers: Arc<Semaphore>,
    replay_worker: Arc<Semaphore>,
}
struct DetachedReplay {
    store: WorkspaceStore,
    snapshot: ReplaySnapshot,
    summary: Option<ReplaySummary>,
    outstanding: Option<(u64, bool)>,
    exhausted: bool,
    finished: bool,
}
enum Cursor {
    Capture(Box<ReplayBuild>),
    Detached(Arc<Mutex<Option<DetachedReplay>>>),
    Discard(Box<WorkDiscard>),
}
struct Entry {
    authority: Arc<Authority>,
    cursor: Option<Cursor>,
    finished: bool,
}
#[derive(Default)]
pub(super) struct Replays {
    entries: HashMap<[u8; 16], Entry>,
    next_reclaim: usize,
    /// Notified when a session drops the ticket of an entry here: the entry
    /// turns reclaimable then, outside the owner, so the owner sleeps on this
    /// instead of polling its table.
    reclaim: Arc<tokio::sync::Notify>,
}
impl Replays {
    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    /// Whether a session has let go of an entry, so
    /// [`State::reclaim_replay_step`] has work.
    pub(super) fn reclaimable(&self) -> bool {
        self.entries.values().any(|entry| !entry.authority.active())
    }
    pub(super) fn reclaim_wake(&self) -> Arc<tokio::sync::Notify> {
        self.reclaim.clone()
    }
}
/// Retired replay work waiting for its discard: at most this many work
/// directories, past the one being discarded.
const RETIRED_BACKLOG: usize = DEFAULT_REPLAY_TICKETS;

/// Discard `retired`, a finished replay's work directory moved aside
/// ([`WorkspaceStore::retire_replay_work`]), on the host's one discarding
/// thread. Unlinked in the owner's turn, a first replay's pins and indexes
/// were most of the turn that cuts its catch-up, in front of every queued
/// job; unlinked by the replay workers alongside the newcomers they pin, the
/// links of the retained frames they share contended. The thread takes one
/// directory at a time; a caller waits while [`RETIRED_BACKLOG`] are queued,
/// so retired work stays bounded. Where no thread can be started, it is
/// discarded here. What a crash leaves, the store's work reclamation removes.
fn discard_retired(retired: WorkDiscard) -> Result<(), StoreError> {
    static QUEUE: OnceLock<Option<std::sync::mpsc::SyncSender<WorkDiscard>>> = OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let (queue, retired) = std::sync::mpsc::sync_channel::<WorkDiscard>(RETIRED_BACKLOG);
        std::thread::Builder::new()
            .name("relay-retired-work".into())
            .spawn(move || {
                for mut discard in retired {
                    let discarding = crate::timing::start();
                    if let Err(error) = discard_whole(&mut discard) {
                        tracing::warn!(%error, "retired replay work was left for reclamation");
                    }
                    crate::timing::window("replay.retired_discard", discarding);
                }
            })
            .ok()
            .map(|_| queue)
    });
    let waiting = crate::timing::start();
    let retired = match queue {
        Some(queue) => match queue.send(retired) {
            Ok(()) => None,
            Err(std::sync::mpsc::SendError(retired)) => Some(retired),
        },
        None => Some(retired),
    };
    crate::timing::window("owner.replay.retired_wait", waiting);
    match retired {
        Some(mut retired) => discard_whole(&mut retired),
        None => Ok(()),
    }
}

fn discard_whole(discard: &mut WorkDiscard) -> Result<(), StoreError> {
    while matches!(discard.step(WorkLimit::default())?, Step::More) {}
    Ok(())
}

/// The process-wide budget every replay and publication shares.
pub fn output_budget() -> &'static ReplayBudget {
    static OUTPUT: OnceLock<ReplayBudget> = OnceLock::new();
    OUTPUT.get_or_init(|| ReplayBudget::new(DEFAULT_OUTPUT_BYTES))
}

impl DetachedReplay {
    /// [`Self::advance`] for up to a slice while it is still preparing, as
    /// an owner transaction runs: one quantum per pool round trip made a
    /// newcomer's replay wait behind every other replay's trips.
    fn advance_slice(
        &mut self,
        authority: &Arc<Authority>,
        mut written: Option<WrittenBatch>,
        budget: &ReplayBudget,
    ) -> Result<ReplayAdvance, StoreError> {
        let start = std::time::Instant::now();
        loop {
            let advance = self.advance(authority, written.take(), budget)?;
            if !matches!(advance, ReplayAdvance::Preparing) || start.elapsed() >= super::SLICE {
                return Ok(advance);
            }
        }
    }
    fn advance(
        &mut self,
        authority: &Arc<Authority>,
        written: Option<WrittenBatch>,
        budget: &ReplayBudget,
    ) -> Result<ReplayAdvance, StoreError> {
        if !authority.active() || authority.session_guard().is_none() || self.finished {
            return Err(error("stale replay ticket/session"));
        }
        match (self.outstanding, written) {
            (Some((number, eof)), Some(WrittenBatch(actual))) if number == actual => {
                self.outstanding = None;
                self.exhausted = eof;
            }
            (None, None) => {}
            _ => return Err(error("invalid replay batch receipt")),
        }
        if self.exhausted {
            return Ok(ReplayAdvance::Exhausted(
                self.summary.invariant("summary missing")?,
            ));
        }
        if self.summary.is_none() {
            match self
                .store
                .prepare_replay_step(&mut self.snapshot, WorkLimit::default())?
            {
                Step::More => return Ok(ReplayAdvance::Preparing),
                Step::Done(summary) => self.summary = Some(summary),
            }
        }
        let batch =
            self.store
                .read_replay_batch(&mut self.snapshot, ReplayLimits::default(), budget)?;
        self.outstanding = Some((batch.number, batch.eof));
        #[cfg(test)]
        crate::store::test_barrier::pause(self.store.root(), "replay-first-batch-ready");
        if super::diagnostics() {
            tracing::info!(
                phase = "replay_batch",
                frames = batch.len(),
                encoded_bytes = batch.encoded_bytes,
                charged_bytes = batch.charged_bytes,
                output_bytes = budget.used(),
                preparation_reads = self.snapshot.stats().preparation_reads,
                hydration_reads = self.snapshot.stats().hydration_reads
            );
        }
        Ok(ReplayAdvance::Batch(batch))
    }
}

impl ReplayHandle {
    fn validate(&self, authority: &Arc<Authority>) -> Result<(), StoreError> {
        let expected = self
            .authority
            .upgrade()
            .ok_or_else(|| error("stale replay handle"))?;
        if !Arc::ptr_eq(&expected, authority)
            || !authority.active()
            || authority.session_guard().is_none()
        {
            return Err(error("stale replay ticket/session"));
        }
        Ok(())
    }

    pub(crate) async fn advance(
        &self,
        authority: &Arc<Authority>,
        mut written: Option<WrittenBatch>,
    ) -> Result<ReplayAdvance, StoreError> {
        self.validate(authority)?;
        loop {
            let waiting = crate::timing::start();
            let replay_permit = self
                .replay_worker
                .clone()
                .acquire_owned()
                .await
                .map_err(|error| super::error(error.to_string()))?;
            let detached = self.detached.clone();
            let authority = authority.clone();
            let acknowledged = written.take();
            let advance = super::blocking(self.workers.clone(), move || {
                let _replay_permit = replay_permit;
                crate::timing::window("replay.worker_wait", waiting);
                let working = crate::timing::start();
                let advanced = detached
                    .lock_recover()
                    .as_mut()
                    .ok_or_else(|| error("stale replay handle"))?
                    .advance_slice(&authority, acknowledged, output_budget());
                crate::timing::window("replay.slice", working);
                advanced
            })
            .await??;
            match advance {
                ReplayAdvance::Preparing => tokio::task::yield_now().await,
                result => return Ok(result),
            }
        }
    }
}
impl State {
    #[cfg(test)]
    pub(crate) fn capture_replay(
        &mut self,
        authority: &Arc<Authority>,
    ) -> Result<Step<ReplayHandle>, StoreError> {
        self.capture_replay_at(authority, None, None)
    }
    /// A replay that leaves out the retained frames the resuming device
    /// already holds.
    pub(crate) fn capture_resumed_replay(
        &mut self,
        authority: &Arc<Authority>,
        skip: Option<crate::store::replay::ResumeSkip>,
    ) -> Result<Step<ReplayHandle>, StoreError> {
        self.capture_replay_at(authority, None, skip)
    }
    /// The final cut of a catch-up: what the workspace committed while it
    /// ran, held under `held`.
    pub(crate) fn capture_pending_replay(
        &mut self,
        authority: &Arc<Authority>,
        held: &std::path::Path,
    ) -> Result<Step<ReplayHandle>, StoreError> {
        self.capture_replay_at(authority, Some(held), None)
    }
    fn capture_replay_at(
        &mut self,
        authority: &Arc<Authority>,
        held: Option<&std::path::Path>,
        skip: Option<crate::store::replay::ResumeSkip>,
    ) -> Result<Step<ReplayHandle>, StoreError> {
        #[cfg(test)]
        if held.is_some() {
            crate::store::test_barrier::pause(self.store.root(), "replay-pending-capture");
        }
        if !authority.active() {
            return Err(error("replay cancelled"));
        }
        if self
            .replays
            .entries
            .get(&authority.id.0)
            .is_some_and(|entry| entry.finished)
        {
            // The replay this one follows is written and cleaned up. Its work
            // directory moves aside in one rename and is discarded off the
            // owner ([`discard_retired`]).
            let retired = crate::timing::windowed("owner.replay.capture_retire", || {
                self.store.retire_replay_work(authority.id)
            })?;
            discard_retired(retired)?;
            self.replays.entries.remove(&authority.id.0);
        }
        if !self.replays.entries.contains_key(&authority.id.0) {
            let build = if let Some(held) = held {
                self.store
                    .start_pending_replay_capture(authority.id, authority.device, held)
                    .map(|build| build.with_every_record(authority.every_record))
            } else {
                self.store
                    .start_replay_capture(authority.id, authority.device)
                    .map(|build| {
                        build
                            .skipping(skip)
                            .with_every_record(authority.every_record)
                    })
            };
            // Its ticket's drop wakes this owner to reclaim the entry. One
            // ticket names one workspace's replay, so it has one owner.
            let _ = authority.reclaim.set(self.replays.reclaim.clone());
            // Register even a failed start so its known reserved root is reclaimed.
            self.replays.entries.insert(
                authority.id.0,
                Entry {
                    authority: authority.clone(),
                    cursor: None,
                    finished: false,
                },
            );
            self.replays
                .entries
                .get_mut(&authority.id.0)
                .invariant("id missing")?
                .cursor = Some(Cursor::Capture(Box::new(build?)));
        }
        let entry = self
            .replays
            .entries
            .get_mut(&authority.id.0)
            .invariant("id missing")?;
        let Some(Cursor::Capture(build)) = &mut entry.cursor else {
            return Err(error("invalid replay capture"));
        };
        #[cfg(test)]
        crate::store::test_barrier::pause(self.store.root(), "replay-prepare");
        let listing = held.and_then(|_| crate::timing::start());
        let step = self
            .store
            .capture_replay_step(build, WorkLimit::default())?;
        // Of a final cut's turn, the listing of what it replays.
        crate::timing::window("owner.replay.pending_capture_list", listing);
        match step {
            Step::More => Ok(Step::More),
            Step::Done(snapshot) => {
                let detached = Arc::new(Mutex::new(Some(DetachedReplay {
                    store: self.store.clone(),
                    snapshot,
                    summary: None,
                    outstanding: None,
                    exhausted: false,
                    finished: false,
                })));
                entry.cursor = Some(Cursor::Detached(detached.clone()));
                Ok(Step::Done(ReplayHandle {
                    detached,
                    authority: Arc::downgrade(authority),
                    workers: self.workers.clone(),
                    replay_worker: self.replay_worker.clone(),
                }))
            }
        }
    }
    fn replay_entry(&mut self, authority: &Arc<Authority>) -> Result<&mut Entry, StoreError> {
        let entry = self
            .replays
            .entries
            .get_mut(&authority.id.0)
            .ok_or_else(|| error("unknown replay ticket"))?;
        if entry.finished
            || !Arc::ptr_eq(&entry.authority, authority)
            || !authority.active()
            || authority.session_guard().is_none()
        {
            return Err(error("stale replay ticket/session"));
        }
        Ok(entry)
    }
    #[cfg(test)]
    pub(crate) fn advance_replay(
        &mut self,
        authority: &Arc<Authority>,
        written: Option<WrittenBatch>,
    ) -> Result<ReplayAdvance, StoreError> {
        self.advance_replay_with_budget(authority, written, output_budget())
    }
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the entry is a borrow of the owner table, not a lock guard"
    )]
    #[cfg(test)]
    fn advance_replay_with_budget(
        &mut self,
        authority: &Arc<Authority>,
        written: Option<WrittenBatch>,
        budget: &ReplayBudget,
    ) -> Result<ReplayAdvance, StoreError> {
        let entry = self.replay_entry(authority)?;
        let Some(Cursor::Detached(detached)) = &entry.cursor else {
            return Err(error("replay is not readable"));
        };
        detached
            .lock_recover()
            .as_mut()
            .ok_or_else(|| error("stale replay handle"))?
            .advance(authority, written, budget)
    }
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the detached replay guard is held until the summary is taken and the entry is marked finished"
    )]
    pub(crate) fn finish_replay(
        &mut self,
        authority: &Arc<Authority>,
        handle: &ReplayHandle,
        authorize: impl FnOnce(&crate::store::WorkspaceStore) -> Result<(), StoreError>,
    ) -> Result<Step<ReplaySummary>, StoreError> {
        handle.validate(authority)?;
        let store = self.store.clone();
        let entry = self.replay_entry(authority)?;
        let Some(Cursor::Detached(current)) = &entry.cursor else {
            return Err(error("replay is not detached"));
        };
        if !Arc::ptr_eq(current, &handle.detached) {
            return Err(error("replay handle changed"));
        }
        let current = current.clone();
        #[cfg(test)]
        crate::store::test_barrier::pause(store.root(), "replay-cleanup");
        let mut detached = current.lock_recover();
        let detached = detached
            .as_mut()
            .ok_or_else(|| error("stale replay handle"))?;
        // The handle wait may cross a session replacement. Refresh routing and
        // liveness after it, immediately before this exact check-and-unlink.
        authorize(&store)?;
        if !authority.active() || authority.session_guard().is_none() {
            return Err(error("replay session changed before cleanup"));
        }
        if !detached.exhausted || detached.outstanding.is_some() {
            return Err(error("replay is not wholly written"));
        }
        match store.cleanup_replay_step(
            &mut detached.snapshot,
            WorkLimit {
                entries: 1,
                ..WorkLimit::default()
            },
        )? {
            Step::More => Ok(Step::More),
            Step::Done(()) => {
                detached.finished = true;
                entry.finished = true;
                Ok(Step::Done(detached.summary.invariant("summary missing")?))
            }
        }
    }
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the entry is a borrow of the owner table, not a lock guard"
    )]
    pub(super) fn reclaim_replay_step(&mut self) -> Result<(), StoreError> {
        // The process-wide permit bounds this table. Step only one entry per
        // quantum, including idle time; cancellation never queues another job.
        // Take the next released entry in turn, past any still in use.
        let n = self.replays.entries.len();
        if n == 0 {
            return Ok(());
        }
        let start = self.replays.next_reclaim.checked_rem(n).unwrap_or(0);
        let Some((offset, id)) = self
            .replays
            .entries
            .iter()
            .cycle()
            .skip(start)
            .take(n)
            .enumerate()
            .find(|(_, (_, entry))| !entry.authority.active())
            .map(|(offset, (id, _))| (offset, *id))
        else {
            return Ok(());
        };
        self.replays.next_reclaim = start.saturating_add(offset).saturating_add(1);
        let entry = self.replays.entries.get_mut(&id).invariant("id missing")?;
        if !matches!(entry.cursor, Some(Cursor::Discard(_))) {
            let discard = match entry.cursor.take() {
                Some(Cursor::Capture(build)) => (*build).into_discard(),
                Some(Cursor::Detached(detached)) => {
                    let retained = detached.clone();
                    let snapshot = match detached.try_lock() {
                        Ok(mut guard) => guard.take().map(|detached| detached.snapshot),
                        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned
                            .into_inner()
                            .take()
                            .map(|detached| detached.snapshot),
                        Err(std::sync::TryLockError::WouldBlock) => {
                            entry.cursor = Some(Cursor::Detached(retained));
                            return Ok(());
                        }
                    };
                    match snapshot {
                        Some(snapshot) => snapshot.into_discard(),
                        None => WorkDiscard::new(
                            self.store
                                .root()
                                .join("store-work-v1")
                                .join(format!("replay-{}", hex::encode(id))),
                        ),
                    }
                }
                _ => WorkDiscard::new(
                    self.store
                        .root()
                        .join("store-work-v1")
                        .join(format!("replay-{}", hex::encode(id))),
                ),
            }?;
            entry.cursor = Some(Cursor::Discard(Box::new(discard)));
        }
        if let Some(Cursor::Discard(discard)) = &mut entry.cursor
            && matches!(discard.step(WorkLimit::default())?, Step::Done(()))
        {
            self.replays.entries.remove(&id);
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
mod tests {
    use super::*;
    use crate::store::replay::DEFAULT_BATCH_BYTES;

    fn capture(state: &mut State, ticket: &ReplayTicket) -> ReplayHandle {
        loop {
            match state.capture_replay(&ticket.authority).unwrap() {
                Step::More => {}
                Step::Done(handle) => return handle,
            }
        }
    }

    #[tokio::test]
    async fn detached_handle_requires_exact_bound_authority_and_rejects_post_finish_advance() {
        let slots = Arc::new(Semaphore::new(2));
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::store::WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let mut state = State {
            store,
            doc: None,
            sequence: None,
            reserved: 0,
            replays: Replays::default(),
            retention_scratch: None,
            retention_candidates: Vec::new(),
            workers: crate::persistence::isolated_test_workers(),
            replay_worker: Arc::new(Semaphore::new(1)),
            retaining_only: false,
            group: crate::persistence::Group::default(),
        };
        let ticket = ReplayTicket::reserve_from([2; 32], None, slots.clone()).unwrap();
        let handle = capture(&mut state, &ticket);
        assert!(handle.advance(&ticket.authority, None).await.is_err());
        ticket
            .authority
            .bind(Arc::new(AtomicBool::new(true)))
            .unwrap();
        let foreign = ReplayTicket::reserve_from([2; 32], None, slots).unwrap();
        foreign
            .authority
            .bind(Arc::new(AtomicBool::new(true)))
            .unwrap();
        assert!(handle.advance(&foreign.authority, None).await.is_err());

        let ReplayAdvance::Batch(batch) = handle.advance(&ticket.authority, None).await.unwrap()
        else {
            panic!("empty replay did not produce its terminal batch")
        };
        let number = batch.number;
        drop(batch);
        assert!(matches!(
            handle
                .advance(&ticket.authority, Some(WrittenBatch(number)))
                .await
                .unwrap(),
            ReplayAdvance::Exhausted(_)
        ));
        while matches!(
            state
                .finish_replay(&ticket.authority, &handle, |_| Ok(()))
                .unwrap(),
            Step::More
        ) {}
        assert!(handle.advance(&ticket.authority, None).await.is_err());
    }

    #[tokio::test]
    async fn detached_cleanup_preserves_same_path_replacement() {
        let slots = Arc::new(Semaphore::new(1));
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::store::WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let raw = crate::transport::encode(&crate::transport::Envelope::Ping).unwrap();
        let device = [2; 32];
        let pending = store.enqueue_pending(&device, 1, &raw).unwrap();
        let mut state = State {
            store,
            doc: None,
            sequence: None,
            reserved: 0,
            replays: Replays::default(),
            retention_scratch: None,
            retention_candidates: Vec::new(),
            workers: crate::persistence::isolated_test_workers(),
            replay_worker: Arc::new(Semaphore::new(1)),
            retaining_only: false,
            group: crate::persistence::Group::default(),
        };
        let ticket = ReplayTicket::reserve_from(device, None, slots).unwrap();
        let handle = capture(&mut state, &ticket);
        ticket
            .authority
            .bind(Arc::new(AtomicBool::new(true)))
            .unwrap();
        let ReplayAdvance::Batch(batch) = handle.advance(&ticket.authority, None).await.unwrap()
        else {
            panic!("pending replay did not produce a batch")
        };
        let number = batch.number;
        drop(batch);
        assert!(matches!(
            handle
                .advance(&ticket.authority, Some(WrittenBatch(number)))
                .await
                .unwrap(),
            ReplayAdvance::Exhausted(_)
        ));
        std::fs::remove_file(&pending).unwrap();
        std::fs::write(&pending, &raw).unwrap();
        while matches!(
            state
                .finish_replay(&ticket.authority, &handle, |_| Ok(()))
                .unwrap(),
            Step::More
        ) {}
        assert!(pending.exists(), "cleanup removed a replacement inode");
    }

    #[test]
    fn cancelled_detached_reclaim_defers_busy_lock_and_recovers_poison() {
        for poison in [false, true] {
            let slots = Arc::new(Semaphore::new(1));
            let tmp = tempfile::tempdir().unwrap();
            let store = crate::store::WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            let mut state = State {
                store,
                doc: None,
                sequence: None,
                reserved: 0,
                replays: Replays::default(),
                retention_scratch: None,
                retention_candidates: Vec::new(),
                workers: crate::persistence::isolated_test_workers(),
                replay_worker: Arc::new(Semaphore::new(1)),
                retaining_only: false,
                group: crate::persistence::Group::default(),
            };
            let ticket = ReplayTicket::reserve_from([2; 32], None, slots.clone()).unwrap();
            let handle = capture(&mut state, &ticket);
            if poison {
                let detached = handle.detached.clone();
                let _ = std::panic::catch_unwind(move || {
                    let _guard = detached.lock().unwrap();
                    panic!("poison detached replay");
                });
                drop(ticket);
            } else {
                let guard = handle.detached.lock().unwrap();
                drop(ticket);
                state.reclaim_replay_step().unwrap();
                assert_eq!(slots.available_permits(), 0);
                drop(guard);
            }
            for _ in 0..1000 {
                if state.replays.is_empty() {
                    break;
                }
                state.reclaim_replay_step().unwrap();
            }
            assert!(state.replays.is_empty());
            assert_eq!(slots.available_permits(), 1);
        }
    }

    #[test]
    fn ticket_admission_includes_cancelled_work_until_bounded_disposal_finishes() {
        // Exercise the production admission primitive with a local pool so this
        // saturation test cannot starve unrelated concurrently running tests.
        let slots = Arc::new(Semaphore::new(DEFAULT_REPLAY_TICKETS));
        let mut held = (0..DEFAULT_REPLAY_TICKETS)
            .map(|_| ReplayTicket::reserve_from([2; 32], None, slots.clone()).unwrap())
            .collect::<Vec<_>>();
        assert!(ReplayTicket::reserve_from([2; 32], None, slots.clone()).is_err());
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::store::WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        for i in 0..257 {
            store
                .enqueue_pending(
                    &[2; 32],
                    i,
                    &crate::transport::encode(&crate::transport::Envelope::Ping).unwrap(),
                )
                .unwrap();
        }
        let mut state = State {
            store,
            doc: None,
            sequence: None,
            reserved: 0,
            replays: Replays::default(),
            retention_scratch: None,
            retention_candidates: Vec::new(),
            workers: crate::persistence::isolated_test_workers(),
            replay_worker: Arc::new(Semaphore::new(1)),
            retaining_only: false,
            group: crate::persistence::Group::default(),
        };
        let ticket = held.pop().unwrap();
        while matches!(state.capture_replay(&ticket.authority).unwrap(), Step::More) {}
        drop(ticket);
        assert_eq!(slots.available_permits(), 0);
        state.reclaim_replay_step().unwrap();
        assert_eq!(
            slots.available_permits(),
            0,
            "ticket admission released before pins were reclaimed"
        );
        for _ in 0..1000 {
            if slots.available_permits() > 0 {
                break;
            }
            state.reclaim_replay_step().unwrap();
        }
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(state.store.count_pending(&[2; 32]).unwrap(), 257);
    }
    /// The replay work directories under the store's work.
    fn replay_dirs(state: &State) -> Vec<String> {
        let work = state.store.root().join(crate::store::replay::WORK_DIR);
        let mut names: Vec<String> = std::fs::read_dir(work)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.starts_with("replay-"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Write `handle`'s replay whole, as the subscriber's connection does.
    async fn write_whole(handle: &ReplayHandle, ticket: &ReplayTicket) -> ReplaySummary {
        let mut written = None;
        loop {
            match handle
                .advance(&ticket.authority, written.take())
                .await
                .unwrap()
            {
                ReplayAdvance::Preparing => {}
                ReplayAdvance::Batch(batch) => written = Some(WrittenBatch(batch.number)),
                ReplayAdvance::Exhausted(summary) => return summary,
            }
        }
    }

    /// A first replay of three queued frames, written and cleaned up, then
    /// the cut of its catch-up, which holds one commit. Returns the first
    /// replay's work directory name, the catch-up and its held commit.
    async fn cut_catch_up(
        state: &mut State,
        ticket: &ReplayTicket,
    ) -> (String, ReplayHandle, std::path::PathBuf) {
        let frame = crate::transport::encode(&crate::transport::Envelope::Ping).unwrap();
        for sequence in 0..3 {
            state
                .store
                .enqueue_pending(&ticket.authority.device, sequence, &frame)
                .unwrap();
        }
        let first = capture(state, ticket);
        ticket
            .authority
            .bind(Arc::new(AtomicBool::new(true)))
            .unwrap();
        write_whole(&first, ticket).await;
        while matches!(
            state
                .finish_replay(&ticket.authority, &first, |_| Ok(()))
                .unwrap(),
            Step::More
        ) {}
        let first_dir = format!("replay-{}", hex::encode(ticket.authority.id.0));
        assert_eq!(replay_dirs(state), std::slice::from_ref(&first_dir));
        let held = state
            .store
            .root()
            .join(crate::store::replay::WORK_DIR)
            .join(crate::store::replay::held_work_name([3; 16]));
        std::fs::create_dir_all(&held).unwrap();
        let commit = held.join(format!(
            "{:016x}-{}.delta",
            9,
            blake3::hash(&frame).to_hex()
        ));
        std::fs::write(&commit, &frame).unwrap();
        let catch_up = loop {
            match state
                .capture_pending_replay(&ticket.authority, &held)
                .unwrap()
            {
                Step::More => {}
                Step::Done(handle) => break handle,
            }
        };
        (first_dir, catch_up, commit)
    }

    #[tokio::test]
    async fn a_catch_up_cut_takes_the_first_replays_name_and_leaves_its_work_to_the_discard() {
        let slots = Arc::new(Semaphore::new(1));
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::store::WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let mut state = State {
            store,
            doc: None,
            sequence: None,
            reserved: 0,
            replays: Replays::default(),
            retention_scratch: None,
            retention_candidates: Vec::new(),
            workers: crate::persistence::isolated_test_workers(),
            replay_worker: Arc::new(Semaphore::new(1)),
            retaining_only: false,
            group: crate::persistence::Group::default(),
        };
        let ticket = ReplayTicket::reserve_from([2; 32], None, slots).unwrap();
        let (first, catch_up, commit) = cut_catch_up(&mut state, &ticket).await;
        assert!(replay_dirs(&state).contains(&first));
        // The first replay's work, moved aside, goes on the discarding thread.
        tilo_test_env::within("the retired replay work was not discarded", async {
            while replay_dirs(&state).len() > 1 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await;
        assert_eq!(replay_dirs(&state), std::slice::from_ref(&first));

        let summary = write_whole(&catch_up, &ticket).await;
        assert_eq!(replay_dirs(&state), [first]);
        assert_eq!(summary.pending_frames, 1);
        assert_eq!(
            summary.cleanup_entries, 0,
            "a held commit went to the cleanup the owner runs"
        );
        assert!(
            commit.exists(),
            "the replay unlinked its held commit itself"
        );
    }

    #[test]
    fn retiring_replay_work_moves_it_aside_whole_for_its_discard() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::store::WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let id = ReplayId([5; 16]);
        let work = store.root().join(crate::store::replay::WORK_DIR);
        let named = work.join(format!("replay-{}", hex::encode(id.0)));
        let pin = std::path::Path::new("pins").join(format!("{:016x}.frame", 0));
        std::fs::create_dir_all(named.join("pins")).unwrap();
        std::fs::write(named.join(&pin), b"pin").unwrap();
        let mut discard = store.retire_replay_work(id).unwrap();
        assert!(
            !named.exists(),
            "the name was not freed for the next capture"
        );
        let moved: Vec<_> = std::fs::read_dir(&work)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        assert_eq!(moved.len(), 1);
        assert!(moved[0].join(&pin).exists(), "the work was not moved whole");
        discard_whole(&mut discard).unwrap();
        assert_eq!(std::fs::read_dir(&work).unwrap().count(), 0);
        // Nothing to move: nothing to discard.
        discard_whole(&mut store.retire_replay_work(id).unwrap()).unwrap();
    }

    #[test]
    fn owner_output_budget_holds_one_oversized_batch_through_write_and_saturation() {
        let slots = Arc::new(Semaphore::new(2));
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::store::WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let frame = crate::transport::encode(&crate::transport::Envelope::Delta {
            workspace_id: 7,
            author_device_public_key: [1; 32],
            authored_at_ns: 1,
            signature: [0; 64],
            delta: vec![255; 2 * 1024 * 1024],
        })
        .unwrap();
        for i in 0..2 {
            store.enqueue_pending(&[2; 32], i, &frame).unwrap();
        }
        let mut state = State {
            store,
            doc: None,
            sequence: None,
            reserved: 0,
            replays: Replays::default(),
            retention_scratch: None,
            retention_candidates: Vec::new(),
            workers: crate::persistence::isolated_test_workers(),
            replay_worker: Arc::new(Semaphore::new(1)),
            retaining_only: false,
            group: crate::persistence::Group::default(),
        };
        let tickets = (0..2)
            .map(|_| {
                let ticket = ReplayTicket::reserve_from([2; 32], None, slots.clone()).unwrap();
                while matches!(state.capture_replay(&ticket.authority).unwrap(), Step::More) {}
                ticket
                    .authority
                    .bind(Arc::new(AtomicBool::new(true)))
                    .unwrap();
                ticket
            })
            .collect::<Vec<_>>();
        // Determine measured capacity from this actual legal frame, then enforce
        // exactly one such output allocation using the same owner state machine.
        let probe = ReplayBudget::new(DEFAULT_OUTPUT_BYTES);
        let first = loop {
            match state
                .advance_replay_with_budget(&tickets[0].authority, None, &probe)
                .unwrap()
            {
                ReplayAdvance::Preparing => {}
                ReplayAdvance::Batch(batch) => break batch,
                ReplayAdvance::Exhausted(_) => panic!("no oversized output"),
            }
        };
        let charge = first.charged_bytes;
        assert_eq!(first.len(), 1);
        assert!(first.encoded_bytes > DEFAULT_BATCH_BYTES);
        drop(first);
        let budget = ReplayBudget::new(charge);
        let held = loop {
            match state
                .advance_replay_with_budget(&tickets[1].authority, None, &budget)
                .unwrap()
            {
                ReplayAdvance::Preparing => {}
                ReplayAdvance::Batch(batch) => break batch,
                ReplayAdvance::Exhausted(_) => panic!("no oversized output"),
            }
        };
        assert_eq!(budget.used(), charge);
        // The first ticket's next batch cannot allocate while the second holds it.
        assert!(
            state
                .advance_replay_with_budget(&tickets[0].authority, Some(WrittenBatch(1)), &budget)
                .is_err()
        );
        assert_eq!(state.store.count_pending(&[2; 32]).unwrap(), 2);
        drop(held);
        assert_eq!(budget.used(), 0);
        let ReplayAdvance::Batch(batch) = state
            .advance_replay_with_budget(&tickets[0].authority, None, &budget)
            .unwrap()
        else {
            panic!("saturation advanced the read offset");
        };
        assert_eq!(batch.len(), 1);
        assert!(batch.eof);
    }
}
