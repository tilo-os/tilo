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
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) struct ReplayTicket {
    pub(crate) authority: Arc<Authority>,
}
impl Drop for ReplayTicket {
    fn drop(&mut self) {
        self.authority.cancelled.store(true, Ordering::SeqCst);
    }
}
pub(crate) struct Authority {
    id: ReplayId,
    device: [u8; 32],
    cancelled: AtomicBool,
    connection: Option<Arc<AtomicBool>>,
    session: Mutex<Option<Arc<AtomicBool>>>,
    // The owner keeps admission during abandoned-spool reclamation.
    _permit: OwnedSemaphorePermit,
}
impl Authority {
    /// The bound session flag. A poisoned lock still holds a coherent
    /// `Option`, so recover it instead of propagating another thread's panic.
    fn session_guard(&self) -> std::sync::MutexGuard<'_, Option<Arc<AtomicBool>>> {
        self.session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
impl ReplayTicket {
    pub(crate) fn reserve(
        device: [u8; 32],
        connection: Option<Arc<AtomicBool>>,
    ) -> Result<Self, StoreError> {
        static TICKETS: OnceLock<Arc<Semaphore>> = OnceLock::new();
        Self::reserve_from(
            device,
            connection,
            TICKETS
                .get_or_init(|| Arc::new(Semaphore::new(DEFAULT_REPLAY_TICKETS)))
                .clone(),
        )
    }
    fn reserve_from(
        device: [u8; 32],
        connection: Option<Arc<AtomicBool>>,
        slots: Arc<Semaphore>,
    ) -> Result<Self, StoreError> {
        let permit = slots
            .try_acquire_owned()
            .map_err(|_| error("replay ticket backpressure"))?;
        Ok(Self {
            authority: Arc::new(Authority {
                id: ReplayId(*uuid::Uuid::new_v4().as_bytes()),
                device,
                cancelled: AtomicBool::new(false),
                connection,
                session: Mutex::new(None),
                _permit: permit,
            }),
        })
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
}
impl Replays {
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
pub(crate) fn output_budget() -> &'static ReplayBudget {
    static OUTPUT: OnceLock<ReplayBudget> = OnceLock::new();
    OUTPUT.get_or_init(|| ReplayBudget::new(DEFAULT_OUTPUT_BYTES))
}

impl DetachedReplay {
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
                detached
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_mut()
                    .ok_or_else(|| error("stale replay handle"))?
                    .advance(&authority, acknowledged, output_budget())
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
        self.capture_replay_at(authority, false, None)
    }
    /// A replay that leaves out the retained frames the resuming device
    /// already holds.
    pub(crate) fn capture_resumed_replay(
        &mut self,
        authority: &Arc<Authority>,
        skip: Option<crate::store::replay::ResumeSkip>,
    ) -> Result<Step<ReplayHandle>, StoreError> {
        self.capture_replay_at(authority, false, skip)
    }
    pub(crate) fn capture_pending_replay(
        &mut self,
        authority: &Arc<Authority>,
    ) -> Result<Step<ReplayHandle>, StoreError> {
        self.capture_replay_at(authority, true, None)
    }
    fn capture_replay_at(
        &mut self,
        authority: &Arc<Authority>,
        pending_only: bool,
        skip: Option<crate::store::replay::ResumeSkip>,
    ) -> Result<Step<ReplayHandle>, StoreError> {
        #[cfg(test)]
        if pending_only {
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
            let entry = self
                .replays
                .entries
                .get_mut(&authority.id.0)
                .invariant("id missing")?;
            if !matches!(entry.cursor, Some(Cursor::Discard(_))) {
                entry.cursor = Some(Cursor::Discard(Box::new(WorkDiscard::new(
                    self.store
                        .root()
                        .join("store-work-v1")
                        .join(format!("replay-{}", hex::encode(authority.id.0))),
                )?)));
            }
            let Some(Cursor::Discard(discard)) = &mut entry.cursor else {
                return Err(error("finished replay disposal cursor missing"));
            };
            let step = discard.step(WorkLimit::default())?;
            match step {
                Step::More => return Ok(Step::More),
                Step::Done(()) => {
                    self.replays.entries.remove(&authority.id.0);
                }
            }
        }
        if !self.replays.entries.contains_key(&authority.id.0) {
            let build = if pending_only {
                self.store
                    .start_pending_replay_capture(authority.id, authority.device)
            } else {
                self.store
                    .start_replay_capture(authority.id, authority.device)
                    .map(|build| build.skipping(skip))
            };
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
        let step = self
            .store
            .capture_replay_step(build, WorkLimit::default())?;
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
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        let mut detached = current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        // The process-wide permit bounds this table. Select only one entry per
        // quantum, including idle time; cancellation never queues another job.
        let n = self.replays.entries.len();
        if n == 0 {
            return Ok(());
        }
        self.replays.next_reclaim = self.replays.next_reclaim.checked_rem(n).unwrap_or(0);
        let id = *self
            .replays
            .entries
            .keys()
            .nth(self.replays.next_reclaim)
            .invariant("next reclaim missing")?;
        self.replays.next_reclaim = self.replays.next_reclaim.saturating_add(1);
        let entry = self.replays.entries.get_mut(&id).invariant("id missing")?;
        if entry.authority.active() {
            return Ok(());
        }
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
