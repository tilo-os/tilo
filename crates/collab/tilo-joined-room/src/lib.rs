//! A device's enrolled replicas of rooms that a remote durable host governs.
//!
//! [`NativeWorkspaceOwner`] holds the admitted state of every room this
//! device enrolled in ([`NativeCommittedView`]) and is the only writer of
//! their durable record, [`NativeAuthorityStorage`]. Every change is admitted
//! into a candidate, published through the host's [`Store`] and only then
//! installed, so no reader or receipt ever sees state the disk may not hold.
//! The desktop session keeps that record inside its state file; the
//! standalone runtime keeps it in a file of its own ([`open_file`]).
//! [`enroll`] redeems an invitation with the host that issued it, and
//! [`session`] holds the relay co-edit session with that host.
mod application_record;
mod bootstrap;
mod certified;
mod document_read;
mod effect_history;
mod effect_receipt;
pub mod enroll;
mod enrollment;
mod file_store;
mod history_content;
mod limits;
mod publication;
mod record;
pub mod session;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
#[cfg(test)]
mod tests;
pub mod voice;
mod workspace_membership;
#[cfg(test)]
tilo_automerge_isolate::test_worker!();

pub use application_record::{ApplicationInitialization, ApplicationRecord};
pub use bootstrap::NativeBootstrap;
pub use certified::{VerifiedNativeEnrollment, VerifiedNativeEnrollmentIntent};
pub use document_read::{CommittedDocument, DocumentRead};
pub use file_store::open_file;
pub use history_content::HistoryContent;
pub use limits::{LimitedWriter, MAX_AUTHORITY_BYTES, MAX_STATE_BYTES, preflight};
pub use publication::{
    NativeOutgoing, NativeOutgoingEntry, preflight_storage, reserved_file_bytes,
    validate_file_layout,
};
pub use record::{
    EnrollmentTarget, NativeAuthorityStorage, PendingEnrollmentId, PendingEnrollmentStatus,
    StoredPendingEnrollment,
};

use parking_lot::{Mutex, RwLock};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Weak},
    time::SystemTime,
};
use tilo_delivery_protocol::descriptor::GenerationExpectation;
use tilo_desktop_model::{RoomId, WorkspaceId};
use tilo_peer_state::{AdmissionReport, NeighbourMsg, StatePeer, TransportOrigin};
use tilo_user_identity::DeviceCert;

/// Where an owner's record is durable.
pub trait Store: std::any::Any + Send + Sync {
    /// Durably retain immutable owner history through the existing content backend.
    /// The state publication may reference the returned address only after this succeeds.
    ///
    /// # Errors
    /// Unsupported storage, quota, integrity or durable I/O failure.
    fn put_history(&self, _bytes: &[u8]) -> Result<[u8; 32], String> {
        Err("native owner history content is unavailable".into())
    }
    /// Read and verify a retained history block. Absence is an error, not UUID absence.
    ///
    /// # Errors
    /// Missing, corrupt, oversized or unavailable content.
    fn read_history(&self, _hash: [u8; 32]) -> Result<Vec<u8>, String> {
        Err("native owner history content is unavailable".into())
    }
    /// False once a publication may or may not have reached disk; the owner
    /// then stays fenced until it is reopened from what the disk holds.
    fn healthy(&self) -> bool;
    /// Whether such an uncertain publication may have made `room` durable.
    fn uncertain(&self, room: RoomId) -> bool;
    /// The room a local workspace slot shows, where the host has slots.
    fn slot_room(&self, slot: WorkspaceId) -> Option<RoomId>;
    /// Make `next` durable, then call `install` before anyone can observe
    /// the new record. Nothing is installed when this fails.
    ///
    /// # Errors
    /// The record, or the host state around it, cannot be published.
    fn publish(
        &self,
        next: &NativeAuthorityStorage,
        change: Change<'_>,
        install: &mut dyn FnMut(),
    ) -> Result<(), String>;
}

/// What a publication changes besides the record itself.
pub enum Change<'a> {
    /// Only pending-enrollment bookkeeping.
    Pending,
    /// `room` now holds `peer`; a completed enrollment binds its slot.
    Room {
        room: RoomId,
        peer: &'a StatePeer,
        bind: Option<&'a EnrollmentTarget>,
    },
}

#[derive(Clone)]
pub struct NativeWorkspaceOwner(Arc<SharedOwner>);
/// Does not keep the owner alive; a host's writer holds one to fence it.
#[derive(Clone)]
pub struct WeakNativeWorkspaceOwner(Weak<SharedOwner>);
struct SharedOwner {
    store: Arc<dyn Store>,
    /// The committed record; replaced only inside a publication's `install`.
    authority: RwLock<NativeAuthorityStorage>,
    mutation: Mutex<()>,
    current: RwLock<Current>,
    listeners: Mutex<Vec<Weak<dyn NativeFenceObserver>>>,
}
#[derive(Default)]
struct Current {
    rooms: BTreeMap<RoomId, Arc<NativeCommittedView>>,
    required: BTreeSet<RoomId>,
    denied: BTreeSet<RoomId>,
    fenced: bool,
}
/// Closure-scoped borrowed view of the actual owner publication. It is never an authority DTO.
#[derive(Clone)]
pub struct NativeCommittedView {
    peer: StatePeer,
    record: record::StoredRoom,
}
impl NativeCommittedView {
    #[must_use]
    pub fn peer(&self) -> &StatePeer {
        &self.peer
    }
    #[must_use]
    pub fn governor(&self) -> &DeviceCert {
        &self.record.governor
    }
    #[must_use]
    pub fn enrollment_invitation(&self) -> &[u8] {
        &self.record.invitation
    }
    #[must_use]
    pub fn device(&self) -> &DeviceCert {
        &self.record.device
    }
    #[must_use]
    pub fn workspace(&self) -> u64 {
        self.record.workspace
    }
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.record.generation
    }
    #[must_use]
    pub fn continuity(&self) -> u64 {
        self.record.continuity
    }
    #[must_use]
    pub fn host(&self) -> Option<&DeviceCert> {
        self.record.host.as_ref()
    }
    #[must_use]
    pub fn delivery_locator(&self) -> Option<&tilo_peer_state::join::TransportHint> {
        self.record.locator.as_ref()
    }
    #[must_use]
    pub fn hint(&self) -> Option<&tilo_peer_state::join::TransportHint> {
        self.record.hint.as_ref()
    }
    #[must_use]
    pub fn transport_hint(&self) -> &tilo_peer_state::join::TransportHint {
        &self.record.transport_hint
    }
    /// # Errors
    /// The enrollment pinned no delivery host independently of its governor.
    pub fn descriptor_expectation(&self) -> Result<GenerationExpectation, String> {
        if self.host().is_none() {
            return Err("enrollment has no independently pinned delivery host".into());
        }
        Ok(match &self.record.floor {
            Some(floor) => GenerationExpectation::Current {
                generation: floor.generation,
                descriptor_digest: floor.digest,
            },
            None => GenerationExpectation::FirstAccepted {
                minimum_generation: 1,
            },
        })
    }
    fn security_digest(&self) -> Result<[u8; 32], String> {
        let policy = self.peer.validated_media_policy().ok().flatten();
        let basis = self
            .peer
            .admitted_room()
            .map(|doc| tilo_peer_state::room::media::security_basis(doc, &self.peer.revocations()))
            .transpose()
            .map_err(|e| e.to_string())?;
        let bytes = postcard::to_stdvec(&(
            policy,
            basis,
            &self.record.device,
            &self.record.host,
            &self.record.hint,
            &self.record.floor,
        ))
        .map_err(|e| e.to_string())?;
        Ok(*blake3::hash(&bytes).as_bytes())
    }
}

/// Callbacks synchronously revoke live client/capture fences; no owner reread or I/O.
pub trait NativeFenceObserver: Send + Sync {
    fn fence(&self, room: RoomId, continuity: u64);
}
impl<F: Fn(RoomId, u64) + Send + Sync> NativeFenceObserver for F {
    fn fence(&self, room: RoomId, continuity: u64) {
        self(room, continuity);
    }
}
/// Retaining this token retains the callback; owner stores only a weak reference.
pub struct NativeFenceSubscription {
    _observer: Arc<dyn NativeFenceObserver>,
}
impl SharedOwner {
    fn notify(&self, room: RoomId, continuity: u64) {
        let observers: Vec<_> = {
            let mut listeners = self.listeners.lock();
            let live = listeners.iter().filter_map(Weak::upgrade).collect();
            listeners.retain(|listener| listener.strong_count() != 0);
            live
        };
        for observer in observers {
            observer.fence(room, continuity);
        }
    }
    fn fence_all(&self) {
        let mut current = self.current.write();
        if current.fenced {
            return;
        }
        current.fenced = true;
        for (room, view) in &current.rooms {
            self.notify(*room, view.continuity());
        }
        drop(current);
        tilo_identity_host::policy_change::announce();
    }
    fn fenced(&self, current: &Current, room: RoomId) -> bool {
        current.fenced || current.denied.contains(&room) || !self.store.healthy()
    }
    /// Publish through the store; an uncertain failure fences every room.
    fn publish(
        &self,
        next: &NativeAuthorityStorage,
        change: Change<'_>,
        install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        let result = self.store.publish(next, change, install);
        if result.is_err() && !self.store.healthy() {
            self.fence_all();
        }
        // A new publication can change a room's tasks, which the broker
        // offers as actions: it re-checks on this rather than on a clock.
        tilo_identity_host::policy_change::announce();
        result
    }
}
impl WeakNativeWorkspaceOwner {
    #[must_use]
    pub fn upgrade(&self) -> Option<NativeWorkspaceOwner> {
        self.0.upgrade().map(NativeWorkspaceOwner)
    }
}
impl NativeWorkspaceOwner {
    /// Restore every enrolled room of `authority`, the record `store` last
    /// published. The owner is not yet shared with anyone.
    ///
    /// # Errors
    /// A pending enrollment or a committed room fails its validation.
    pub fn restore(
        store: Arc<dyn Store>,
        authority: NativeAuthorityStorage,
        now: SystemTime,
    ) -> Result<Self, String> {
        if authority.pending.len() > limits::MAX_PENDING {
            return Err("native pending enrollment quota exceeded".into());
        }
        let mut pending_slots = BTreeSet::new();
        for pending in authority.pending.values() {
            if let Some(target) = &pending.target
                && !pending_slots.insert(target.workspace)
            {
                return Err("ambiguous native pending enrollment slot".into());
            }
        }
        let mut current = Current::default();
        current.required.extend(authority.rooms.keys().copied());
        current.required.extend(authority.pending.keys().copied());
        for (room, pending) in &authority.pending {
            enrollment::validate_pending(*room, pending)?;
        }
        for (room, record) in &authority.rooms {
            let view = enrollment::restore(*room, record, now)?;
            effect_receipt::validate_history(&view, store.as_ref())?;
            current.rooms.insert(*room, Arc::new(view));
        }
        Ok(Self(Arc::new(SharedOwner {
            store,
            authority: RwLock::new(authority),
            mutation: Mutex::new(()),
            current: RwLock::new(current),
            listeners: Mutex::new(Vec::new()),
        })))
    }
    #[must_use]
    pub fn downgrade(&self) -> WeakNativeWorkspaceOwner {
        WeakNativeWorkspaceOwner(Arc::downgrade(&self.0))
    }
    /// Fence every room: the host's store lost certainty about the disk.
    pub fn fence_all(&self) {
        self.0.fence_all();
    }
    /// Every room with a committed enrollment.
    #[must_use]
    pub fn rooms(&self) -> Vec<RoomId> {
        self.0.current.read().rooms.keys().copied().collect()
    }
    #[must_use]
    pub fn contains_room(&self, room: RoomId) -> bool {
        self.0.current.read().required.contains(&room) || self.0.store.uncertain(room)
    }

    /// Canonical task state from the current admitted owner view, never the
    /// desktop presentation cache.
    ///
    /// # Errors
    /// The room is fenced, absent, or its task-control sidecar is malformed.
    pub fn workspace_task_snapshot(
        &self,
        room: RoomId,
    ) -> Result<tilo_peer_state::workspace_task::WorkspaceTaskSnapshot, String> {
        self.with_current(room, |view| {
            let document = view
                .peer()
                .admitted_room()
                .ok_or("native room has no admitted task document")?;
            tilo_peer_state::workspace_task::snapshot(document)
        })
    }

    /// Result retained by the owner for one device-scoped task invocation.
    ///
    /// # Errors
    /// The room is fenced, absent, or its task-control sidecar is malformed.
    pub fn workspace_task_result(
        &self,
        room: RoomId,
        id: tilo_peer_state::workspace_task::WorkspaceTaskCommandId,
    ) -> Result<Option<tilo_desktop_model::WorkspaceTaskMutationResult>, String> {
        self.with_current(room, |view| {
            let document = view
                .peer()
                .admitted_room()
                .ok_or("native room has no admitted task document")?;
            tilo_peer_state::workspace_task::result_of(document, id)
        })
    }

    /// Serialize one ordinary agent action admission against task directions.
    /// The current room read guard is released before `admit` runs, while the
    /// task mutation turn remains held, so nested policy reads are safe and a
    /// concurrent direction is ordered wholly before or after this admission.
    ///
    /// `applied_direction_revision` is the revision the agent actually planned
    /// against. It must equal both the active run's acknowledgement and the
    /// latest durable direction, preventing a delayed pre-direction action from
    /// entering after a newer direction has been acknowledged.
    ///
    /// # Errors
    /// The room is fenced, the task/run/revision lease is stale, or `admit`
    /// refuses the ordinary invocation.
    pub fn with_task_action_admission<T>(
        &self,
        room: RoomId,
        task_id: &str,
        agent_id: &str,
        run_epoch: u64,
        applied_direction_revision: u64,
        admit: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _turn = self.0.mutation.lock();
        self.with_current(room, |view| {
            let document = view
                .peer()
                .admitted_room()
                .ok_or("native room has no admitted task document")?;
            let snapshot = tilo_peer_state::workspace_task::snapshot(document)?;
            let task = snapshot
                .tasks
                .iter()
                .find(|task| task.id == task_id)
                .ok_or("workspace task no longer exists")?;
            let control = snapshot
                .controls
                .get(task_id)
                .ok_or("workspace task has no execution control")?;
            let run = control
                .active_run
                .as_ref()
                .ok_or("workspace task has no active run")?;
            let latest_direction = control
                .directions
                .last()
                .map_or(0, |direction| direction.revision);
            if task.status != tilo_desktop_model::WorkspaceTaskStatus::InProgress
                || task.assigned_agent.as_deref() != Some(agent_id)
                || run.agent_id != agent_id
                || run.epoch != run_epoch
                || run.applied_direction_revision != applied_direction_revision
                || latest_direction != applied_direction_revision
            {
                return Err("workspace task action lease is stale".into());
            }
            view.peer().check_task_execution_actor(
                agent_id,
                run.approval.as_ref(),
                std::time::SystemTime::now(),
            )?;
            Ok(())
        })?;
        admit()
    }
    /// Store availability only; room authority still requires `with_current`.
    ///
    /// # Errors
    /// The owner is fenced.
    pub fn health(&self) -> Result<(), String> {
        if self.0.current.read().fenced || !self.0.store.healthy() {
            return Err("native owner continuity is fenced".into());
        }
        Ok(())
    }
    /// # Errors
    /// The room is fenced or has no committed enrollment, or `read` fails.
    pub fn with_current<T>(
        &self,
        room: RoomId,
        read: impl FnOnce(&NativeCommittedView) -> Result<T, String>,
    ) -> Result<T, String> {
        let current = self.0.current.read();
        if self.0.fenced(&current, room) {
            return Err("native owner continuity is fenced".into());
        }
        read(
            current
                .rooms
                .get(&room)
                .ok_or("room has no committed native enrollment")?,
        )
    }
    pub fn observe_fences(
        &self,
        observer: Arc<dyn NativeFenceObserver>,
    ) -> NativeFenceSubscription {
        self.0.listeners.lock().push(Arc::downgrade(&observer));
        NativeFenceSubscription {
            _observer: observer,
        }
    }
    #[allow(
        clippy::significant_drop_tightening,
        reason = "fence observers are notified in order under the continuity write lock"
    )]
    pub fn invalidate_identity(&self, room: RoomId) {
        let mut current = self.0.current.write();
        if !current.denied.insert(room) {
            return;
        }
        let continuity = current.rooms.get(&room).map_or(0, |view| view.continuity());
        self.0.notify(room, continuity);
        drop(current);
        tilo_identity_host::policy_change::announce();
    }
    /// Admits genuine relay provenance; pending document prerequisites are also durable.
    ///
    /// # Errors
    /// The message is not workspace state, the room is fenced, admission
    /// refuses it or the result cannot be published.
    pub fn receive(
        &self,
        room: RoomId,
        origin: TransportOrigin,
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<AdmissionReport, String> {
        if !matches!(
            message,
            NeighbourMsg::SyncResp { .. } | NeighbourMsg::AppDocument { .. }
        ) {
            return Err("native owner admits only workspace state messages".into());
        }
        let _turn = self.0.mutation.lock();
        if matches!(&message, NeighbourMsg::AppDocument { .. })
            && !matches!(origin, TransportOrigin::RetainedNativePublication { .. })
            && let Some(report) = self.with_current(room, |view| {
                let exact = view
                    .peer
                    .lookup_exact_document_report(origin, &message, now)
                    .map_err(|error| error.to_string())?;
                if exact.is_some() && publication::exact_replay_unchanged(view)? {
                    Ok(exact)
                } else {
                    Ok(None)
                }
            })?
        {
            return Ok(report);
        }
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        let report = candidate
            .peer
            .receive_from_report(origin, message, now)
            .map_err(|e| e.to_string())?;
        let previous_publication = candidate.record.publication.clone();
        publication::retire(&mut candidate)?;
        publication::refresh_governance_checkpoint(&mut candidate)?;
        let durable = candidate.peer.durable()?;
        // Authentication and causal admission still run for every replay.
        // Reuse the actual committed barrier only when both durable peer
        // state and outgoing custody are byte-for-byte unchanged. In
        // particular, newly settled prerequisites or retired obligations
        // still require publication before they can be observed.
        if durable == candidate.record.peer && candidate.record.publication == previous_publication
        {
            return Ok(report);
        }
        candidate.record.peer = durable;
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_room(room, candidate, None)?;
        Ok(report)
    }
    /// Caller holds the serialized turn, but readers keep the preceding committed view during I/O.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "fence observers are notified in order under the continuity write lock"
    )]
    fn publish_room(
        &self,
        room: RoomId,
        mut candidate: NativeCommittedView,
        enrollment: Option<&StoredPendingEnrollment>,
    ) -> Result<(), String> {
        publication::refresh_governance_checkpoint(&mut candidate)?;
        let candidate_security = candidate.security_digest()?;
        let changed = {
            let current = self.0.current.read();
            if self.0.fenced(&current, room) {
                return Err("native owner continuity is fenced".into());
            }
            current
                .rooms
                .get(&room)
                .map(|old| {
                    old.security_digest()
                        .map(|digest| digest != candidate_security)
                })
                .transpose()?
                .unwrap_or(true)
        };
        limits::preflight(&candidate.record, limits::MAX_AUTHORITY_BYTES)
            .map_err(|e| e.to_string())?;
        if changed {
            candidate.record.continuity = candidate
                .record
                .continuity
                .checked_add(1)
                .ok_or("native continuity exhausted")?;
        }
        let mut next = self.0.authority.read().clone();
        if let Some(pending) = enrollment
            && next.pending.get(&room).is_none_or(|current| {
                current.id != pending.id || current.status != PendingEnrollmentStatus::Active
            })
        {
            return Err("enrollment target or pending generation changed before completion".into());
        }
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or("native storage generation exhausted")?;
        next.rooms.insert(room, candidate.record.clone());
        if enrollment.is_some() {
            next.pending.remove(&room);
        }
        let candidate = Arc::new(candidate);
        self.0.publish(
            &next,
            Change::Room {
                room,
                peer: &candidate.peer,
                bind: enrollment.and_then(|pending| pending.target.as_ref()),
            },
            &mut || {
                let mut current = self.0.current.write();
                if changed {
                    self.0.notify(room, candidate.continuity());
                }
                current.required.insert(room);
                current.rooms.insert(room, Arc::clone(&candidate));
                *self.0.authority.write() = next.clone();
            },
        )
    }
}
