//! Session authority belongs to the ordered owner, never to a persisted manifest.
use super::{State, error};
use crate::store::{StoreError, replay::*};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock,
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
    fn active(&self) -> bool {
        !self.cancelled.load(Ordering::SeqCst)
            && self
                .connection
                .as_ref()
                .is_none_or(|live| live.load(Ordering::SeqCst))
            && self
                .session
                .lock()
                .unwrap()
                .as_ref()
                .is_none_or(|live| live.load(Ordering::SeqCst))
    }
    pub(crate) fn bind(&self, live: Arc<AtomicBool>) -> Result<(), StoreError> {
        let mut session = self.session.lock().unwrap();
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
                .session
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|bound| Arc::ptr_eq(bound, live))
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
enum Cursor {
    Capture(ReplayBuild),
    Snapshot(ReplaySnapshot),
    Discard(WorkDiscard),
}
struct Entry {
    authority: Arc<Authority>,
    cursor: Option<Cursor>,
    summary: Option<ReplaySummary>,
    outstanding: Option<(u64, bool)>,
    exhausted: bool,
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
impl State {
    pub(crate) fn capture_replay(
        &mut self,
        authority: &Arc<Authority>,
    ) -> Result<Step<()>, StoreError> {
        if !authority.active() {
            return Err(error("replay cancelled"));
        }
        if !self.replays.entries.contains_key(&authority.id.0) {
            let build = self
                .store
                .start_replay_capture(authority.id, authority.device);
            // Register even a failed start so its known reserved root is reclaimed.
            self.replays.entries.insert(
                authority.id.0,
                Entry {
                    authority: authority.clone(),
                    cursor: None,
                    summary: None,
                    outstanding: None,
                    exhausted: false,
                    finished: false,
                },
            );
            self.replays
                .entries
                .get_mut(&authority.id.0)
                .unwrap()
                .cursor = Some(Cursor::Capture(build?));
        }
        let entry = self.replays.entries.get_mut(&authority.id.0).unwrap();
        let Some(Cursor::Capture(build)) = &mut entry.cursor else {
            return Err(error("invalid replay capture"));
        };
        #[cfg(test)]
        crate::store::test_barrier::pause(self.store.root(), "replay-prepare");
        match self
            .store
            .capture_replay_step(build, WorkLimit::default())?
        {
            Step::More => Ok(Step::More),
            Step::Done(snapshot) => {
                entry.cursor = Some(Cursor::Snapshot(snapshot));
                Ok(Step::Done(()))
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
            || authority.session.lock().unwrap().is_none()
        {
            return Err(error("stale replay ticket/session"));
        }
        Ok(entry)
    }
    pub(crate) fn advance_replay(
        &mut self,
        authority: &Arc<Authority>,
        written: Option<WrittenBatch>,
    ) -> Result<ReplayAdvance, StoreError> {
        self.advance_replay_with_budget(authority, written, output_budget())
    }
    fn advance_replay_with_budget(
        &mut self,
        authority: &Arc<Authority>,
        written: Option<WrittenBatch>,
        budget: &ReplayBudget,
    ) -> Result<ReplayAdvance, StoreError> {
        let store = self.store.clone();
        let entry = self.replay_entry(authority)?;
        match (entry.outstanding, written) {
            (Some((number, eof)), Some(WrittenBatch(actual))) if number == actual => {
                entry.outstanding = None;
                entry.exhausted = eof;
            }
            (None, None) => {}
            _ => return Err(error("invalid replay batch receipt")),
        }
        if entry.exhausted {
            return Ok(ReplayAdvance::Exhausted(entry.summary.unwrap()));
        }
        let Some(Cursor::Snapshot(snapshot)) = &mut entry.cursor else {
            return Err(error("replay is not readable"));
        };
        if entry.summary.is_none() {
            match store.prepare_replay_step(snapshot, WorkLimit::default())? {
                Step::More => return Ok(ReplayAdvance::Preparing),
                Step::Done(summary) => entry.summary = Some(summary),
            }
        }
        let batch = store.read_replay_batch(snapshot, ReplayLimits::default(), budget)?;
        entry.outstanding = Some((batch.number, batch.eof));
        #[cfg(test)]
        crate::store::test_barrier::pause(store.root(), "replay-first-batch-ready");
        if super::diagnostics() {
            tracing::info!(
                phase = "replay_batch",
                frames = batch.len(),
                encoded_bytes = batch.encoded_bytes,
                charged_bytes = batch.charged_bytes,
                output_bytes = budget.used(),
                preparation_reads = snapshot.stats().preparation_reads,
                hydration_reads = snapshot.stats().hydration_reads
            );
        }
        Ok(ReplayAdvance::Batch(batch))
    }
    pub(crate) fn finish_replay(
        &mut self,
        authority: &Arc<Authority>,
        authorize: impl FnOnce(&crate::store::WorkspaceStore) -> Result<(), StoreError>,
    ) -> Result<Step<ReplaySummary>, StoreError> {
        let store = self.store.clone();
        let entry = self.replay_entry(authority)?;
        if !entry.exhausted || entry.outstanding.is_some() {
            return Err(error("replay is not wholly written"));
        }
        #[cfg(test)]
        crate::store::test_barrier::pause(store.root(), "replay-cleanup");
        authorize(&store)?;
        // Authorization was refreshed at this job's ordered preparation. Recheck
        // exact liveness after waits, immediately before this single-entry unlink.
        if !authority.active() {
            return Err(error("replay session changed before cleanup"));
        }
        let Some(Cursor::Snapshot(snapshot)) = &mut entry.cursor else {
            return Err(error("replay already finished"));
        };
        match store.cleanup_replay_step(
            snapshot,
            WorkLimit {
                entries: 1,
                ..WorkLimit::default()
            },
        )? {
            Step::More => Ok(Step::More),
            Step::Done(()) => {
                entry.finished = true;
                Ok(Step::Done(entry.summary.unwrap()))
            }
        }
    }
    pub(super) fn reclaim_replay_step(&mut self) -> Result<(), StoreError> {
        // The process-wide permit bounds this table. Select only one entry per
        // quantum, including idle time; cancellation never queues another job.
        let n = self.replays.entries.len();
        if n == 0 {
            return Ok(());
        }
        self.replays.next_reclaim %= n;
        let id = *self
            .replays
            .entries
            .keys()
            .nth(self.replays.next_reclaim)
            .unwrap();
        self.replays.next_reclaim += 1;
        let entry = self.replays.entries.get_mut(&id).unwrap();
        if entry.authority.active() {
            return Ok(());
        }
        if !matches!(entry.cursor, Some(Cursor::Discard(_))) {
            let discard = match entry.cursor.take() {
                Some(Cursor::Capture(build)) => build.into_discard(),
                Some(Cursor::Snapshot(snapshot)) => snapshot.into_discard(),
                _ => WorkDiscard::new(
                    self.store
                        .root()
                        .join("store-work-v1")
                        .join(format!("replay-{}", hex::encode(id))),
                ),
            }?;
            entry.cursor = Some(Cursor::Discard(discard));
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
mod tests {
    use super::*;
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
            replays: Replays::default(),
            retention_scratch: None,
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
            replays: Replays::default(),
            retention_scratch: None,
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
                _ => panic!("no oversized output"),
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
                _ => panic!("no oversized output"),
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
