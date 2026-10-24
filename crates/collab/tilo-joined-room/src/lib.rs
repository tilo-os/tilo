//! A device's rooms: replicas of rooms a remote durable host governs, and
//! rooms this device governs itself.
//!
//! [`NativeWorkspaceOwner`] holds the admitted state of every room this
//! device enrolled in or created ([`NativeCommittedView`]) and is the only writer of
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
mod checkpoint;
mod document_read;
mod effect_history;
mod effect_receipt;
pub mod enroll;
mod enrollment;
mod file_store;
mod history_content;
mod limits;
mod private_direct_conversations;
mod publication;
mod record;
pub mod renewal;
pub mod session;
mod shortcuts;
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
pub use checkpoint::{
    CheckpointAcknowledgement, CheckpointCapture, CheckpointEntry, CheckpointRoute,
    CheckpointSchema, MaterializedCheckpoint, NativeCheckpoint,
};
pub use document_read::{ChangesRead, CommittedDocument, DocumentChanges, DocumentRead};
pub use file_store::open_file;
pub use history_content::HistoryContent;
pub use limits::{LimitedWriter, MAX_AUTHORITY_BYTES, MAX_STATE_BYTES, preflight};
pub use private_direct_conversations::{
    DirectContact, DirectConversation, DirectConversationPage, DirectMessage, DirectMessageReceipt,
    DirectMessageSend, DirectMessageStatus, PrivateDirectConversationStore,
};
pub use publication::{
    NativeOutgoing, NativeOutgoingEntry, authority_limit, preflight_storage, reserved_file_bytes,
    validate_file_layout,
};
pub use record::{
    EnrollmentTarget, NativeAuthorityStorage, PendingEnrollmentId, PendingEnrollmentStatus,
    StoredPendingEnrollment,
};

use neighbour_wire::NeighbourMsg;
use parking_lot::{Mutex, RwLock};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Weak},
    time::SystemTime,
};
use tilo_delivery_protocol::descriptor::GenerationExpectation;
use tilo_desktop_model::{RoomId, WorkspaceId};
use tilo_peer_state::{AdmissionReport, StatePeer, TransportOrigin};
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
    /// The room a local workspace is, where the host has workspaces.
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
    /// `room` now holds `peer`; a completed enrollment installs into the
    /// workspace made for it, which is `room` already.
    Room {
        room: RoomId,
        peer: &'a StatePeer,
        into: Option<&'a EnrollmentTarget>,
    },
    /// This device no longer holds `room`: its replica and any enrollment
    /// into it are gone from the record, and the host takes the room's
    /// workspace off in the same publication. Nobody else's data goes: the
    /// room lives on with its governor and members.
    Released { room: RoomId },
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
    /// Process-local private pair notifications, never serialized or replicated.
    direct_changes: Mutex<BTreeMap<String, tokio::sync::watch::Sender<u64>>>,
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
    /// Who governs the room now: the invitation's governor, or the device
    /// its root named in its place.
    #[must_use]
    pub fn governor(&self) -> &DeviceCert {
        self.peer.authority()
    }
    /// The signed invitation this device enrolled with.
    ///
    /// # Errors
    /// The room's record holds no enrollment.
    pub fn enrollment_invitation(&self) -> Result<&[u8], String> {
        Ok(&self.enrollment()?.invitation)
    }
    fn enrollment(&self) -> Result<&record::StoredEnrollment, String> {
        self.record
            .enrollment
            .as_ref()
            .ok_or_else(|| "the room's record holds no enrollment".into())
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
        self.record.enrollment.as_ref()?.host.as_ref()
    }
    #[must_use]
    pub fn delivery_locator(&self) -> Option<&tilo_peer_state::join::TransportHint> {
        self.record.enrollment.as_ref()?.locator.as_ref()
    }
    #[must_use]
    pub fn hint(&self) -> Option<&tilo_peer_state::join::TransportHint> {
        self.record.enrollment.as_ref()?.hint.as_ref()
    }
    /// Where the host this device enrolled through is reached.
    ///
    /// # Errors
    /// The room's record holds no enrollment.
    pub fn transport_hint(&self) -> Result<&tilo_peer_state::join::TransportHint, String> {
        Ok(&self.enrollment()?.transport_hint)
    }
    /// # Errors
    /// The enrollment pinned no delivery host independently of its governor.
    pub fn descriptor_expectation(&self) -> Result<GenerationExpectation, String> {
        if self.host().is_none() {
            return Err("enrollment has no independently pinned delivery host".into());
        }
        Ok(match &self.enrollment()?.floor {
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
        let enrollment = self.record.enrollment.as_ref();
        let bytes = postcard::to_stdvec(&(
            policy,
            basis,
            &self.record.device,
            enrollment.and_then(|enrollment| enrollment.host.as_ref()),
            enrollment.and_then(|enrollment| enrollment.hint.as_ref()),
            enrollment.and_then(|enrollment| enrollment.floor.as_ref()),
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
impl Drop for SharedOwner {
    fn drop(&mut self) {
        // Rooms that required this owner have none now.
        tilo_identity_host::policy_change::announce();
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
            checkpoint::validate_restore(&view, store.as_ref())?;
            current.rooms.insert(*room, Arc::new(view));
        }
        Ok(Self(Arc::new(SharedOwner {
            store,
            authority: RwLock::new(authority),
            mutation: Mutex::new(()),
            current: RwLock::new(current),
            listeners: Mutex::new(Vec::new()),
            direct_changes: Mutex::new(BTreeMap::new()),
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
    /// Stop holding `room` here: drop this device's replica of it and any
    /// enrollment into it, with the host's workspace of it in the same
    /// publication ([`Change::Released`]). A workspace and its room go
    /// together; the room itself, its governor and its other members are
    /// untouched. Answers whether the record held the room.
    ///
    /// # Errors
    /// The record cannot be published.
    pub fn release_room(&self, room: RoomId) -> Result<bool, String> {
        let _turn = self.0.mutation.lock();
        let mut next = self.0.authority.read().clone();
        let held = next.rooms.remove(&room).is_some();
        let joining = next.pending.remove(&room).is_some();
        if !held && !joining {
            return Ok(false);
        }
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or("native storage generation exhausted")?;
        let mut continuity = 0;
        self.0.publish(&next, Change::Released { room }, &mut || {
            {
                let mut current = self.0.current.write();
                continuity = current
                    .rooms
                    .remove(&room)
                    .map_or(0, |view| view.continuity());
                current.required.remove(&room);
                current.denied.remove(&room);
            }
            *self.0.authority.write() = next.clone();
        })?;
        // Every session of the room ends: its replica is gone. Told once the
        // publication is over, with no lock of the host's held.
        self.0.notify(room, continuity);
        Ok(true)
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

    /// Store availability only; room authority still requires `with_current`.
    pub fn health(&self) -> Result<(), String> {
        if self.0.current.read().fenced || !self.0.store.healthy() {
            return Err("native owner continuity is fenced".into());
        }
        Ok(())
    }
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
    /// Move every room whose governor its root revoked to the device the
    /// root named in its place ([`StatePeer::follow_successions`]), and keep
    /// the statements with the room so it is restored under the successor.
    /// Returns the rooms that moved.
    ///
    /// # Errors
    /// A room is fenced, refuses the successor, or cannot be published.
    pub fn follow_successions(
        &self,
        successions: &[tilo_user_identity::GovernorSuccession],
        now: SystemTime,
    ) -> Result<Vec<RoomId>, String> {
        if successions.is_empty() {
            return Ok(Vec::new());
        }
        let _turn = self.0.mutation.lock();
        let rooms: Vec<RoomId> = self
            .0
            .current
            .read()
            .rooms
            .iter()
            .filter(|(_, view)| {
                tilo_peer_state::succeeded_authority(view.peer.authority(), successions, now)
                    .is_some()
            })
            .map(|(room, _)| *room)
            .collect();
        let mut moved = Vec::new();
        for room in rooms {
            let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
            if !candidate.peer.follow_successions(successions, now)? {
                continue;
            }
            let root = candidate.record.governor.user_public_key;
            for succession in successions {
                if succession.previous.user_public_key == root
                    && !candidate.record.successions.contains(succession)
                {
                    candidate.record.successions.push(succession.clone());
                }
            }
            candidate.record.peer = candidate.peer.durable()?;
            candidate.record.generation = candidate
                .record
                .generation
                .checked_add(1)
                .ok_or("native owner generation exhausted")?;
            self.publish_room(room, candidate, None)?;
            moved.push(room);
        }
        Ok(moved)
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
        self.receive_batch(room, vec![(origin, message)], now)?
            .pop()
            .ok_or_else(|| "native owner admitted no input".to_owned())?
    }
    /// Admit `inputs`, in order, into `room` and commit the room once for
    /// all of them: a host's replay of a busy room is thousands of inputs,
    /// and committing the room for each one rewrote the whole state file
    /// thousands of times. Each input is decided on its own, exactly as
    /// [`Self::receive`] decides it, and its refusal changes nothing; the
    /// answer is one result per input. Nothing of the batch is observable,
    /// and no report is returned, before the one commit is durable: when
    /// it fails, every input fails with it.
    ///
    /// # Errors
    /// The room is fenced, or the batch's commit cannot be published.
    pub fn receive_batch(
        &self,
        room: RoomId,
        inputs: Vec<(TransportOrigin, NeighbourMsg)>,
        now: SystemTime,
    ) -> Result<Vec<Result<AdmissionReport, String>>, String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        let previous_publication = candidate.record.publication.clone();
        let mut reports = Vec::with_capacity(inputs.len());
        for (origin, message) in inputs {
            reports.push(Self::admit_input(
                &mut candidate,
                room,
                origin,
                message,
                now,
            ));
        }
        if reports.iter().all(Result::is_err) {
            // Every input was refused: nothing changed to commit.
            return Ok(reports);
        }
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
            return Ok(reports);
        }
        candidate.record.peer = durable;
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_room(room, candidate, None)?;
        Ok(reports)
    }
    /// Decide one relay input into `candidate`: refused, it changes nothing.
    fn admit_input(
        candidate: &mut NativeCommittedView,
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
        if matches!(&message, NeighbourMsg::AppDocument { .. })
            && !matches!(origin, TransportOrigin::RetainedNativePublication { .. })
        {
            let exact = candidate
                .peer
                .lookup_exact_document_report(origin, &message, now)
                .map_err(|error| error.to_string())?;
            if let Some(exact) = exact
                && publication::exact_replay_unchanged(candidate)?
            {
                return Ok(exact);
            }
        }
        let report = candidate
            .peer
            .receive_from_report(origin, message, now)
            .map_err(|e| e.to_string())?;
        for dropped in &report.summary().dropped {
            tracing::warn!(%room, %dropped, "a held app message was let go unapplied");
        }
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
                into: enrollment.and_then(|pending| pending.target.as_ref()),
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
