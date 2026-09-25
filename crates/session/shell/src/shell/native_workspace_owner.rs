//! Actual native semantic owner, committed through the existing state.json writer.
mod bootstrap;
mod enrollment;
pub(crate) mod publication;
mod record;
pub(crate) mod store;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::{test_bootstrap_response, test_descriptor, test_enrolled_owner};

use super::{
    persist,
    state::{RoomId, WorkspaceTreeHandle},
};
pub(crate) use bootstrap::NativeBootstrap;
use parking_lot::{Mutex, RwLock};
pub(crate) use publication::{NativeOutgoing, NativeOutgoingEntry, NativeOutgoingStatus};
use qos_peer_state::{AdmissionReport, NeighbourMsg, StatePeer, TransportOrigin};
pub use record::NativeAuthorityStorage;
pub(crate) use record::{PendingEnrollmentId, PendingEnrollmentStatus, StoredPendingEnrollment};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Weak},
    time::SystemTime,
};
pub use store::NativeStateRuntime;
use tilo_relay::delivery_v3::descriptor::GenerationExpectation;
use tilo_user_identity::DeviceCert;

#[derive(Clone)]
pub(crate) struct NativeWorkspaceOwner(Arc<SharedOwner>);
struct SharedOwner {
    tree: WorkspaceTreeHandle,
    runtime: NativeStateRuntime,
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
pub(crate) struct NativeCommittedView {
    peer: StatePeer,
    record: record::StoredRoom,
}
impl NativeCommittedView {
    pub(crate) fn peer(&self) -> &StatePeer {
        &self.peer
    }
    pub(crate) fn governor(&self) -> &DeviceCert {
        &self.record.governor
    }
    pub(crate) fn enrollment_invitation(&self) -> &[u8] {
        &self.record.invitation
    }
    pub(crate) fn device(&self) -> &DeviceCert {
        &self.record.device
    }
    pub(crate) fn workspace(&self) -> u64 {
        self.record.workspace
    }
    pub(crate) fn generation(&self) -> u64 {
        self.record.generation
    }
    pub(crate) fn continuity(&self) -> u64 {
        self.record.continuity
    }
    pub(crate) fn host(&self) -> Option<&DeviceCert> {
        self.record.host.as_ref()
    }
    pub(crate) fn delivery_locator(&self) -> Option<&qos_peer_state::join::TransportHint> {
        self.record.locator.as_ref()
    }
    pub(crate) fn hint(&self) -> Option<&qos_peer_state::join::TransportHint> {
        self.record.hint.as_ref()
    }
    pub(crate) fn transport_hint(&self) -> &qos_peer_state::join::TransportHint {
        &self.record.transport_hint
    }
    pub(crate) fn descriptor_expectation(&self) -> Result<GenerationExpectation, String> {
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
            .map(|doc| qos_peer_state::room::media::security_basis(doc, &self.peer.revocations()))
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
pub(crate) trait NativeFenceObserver: Send + Sync {
    fn fence(&self, room: RoomId, continuity: u64);
}
impl<F: Fn(RoomId, u64) + Send + Sync> NativeFenceObserver for F {
    fn fence(&self, room: RoomId, continuity: u64) {
        self(room, continuity);
    }
}
/// Retaining this token retains the callback; owner stores only a weak reference.
pub(crate) struct NativeFenceSubscription {
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
        current.fenced = true;
        for (room, view) in &current.rooms {
            self.notify(*room, view.continuity());
        }
    }
}
impl NativeWorkspaceOwner {
    pub(crate) fn open(tree: WorkspaceTreeHandle) -> Result<Self, String> {
        let mut native = tree.write();
        if let Some(owner) = native.installed_native_owner() {
            if !Arc::ptr_eq(&owner.0.tree, &tree) {
                return Err(
                    "state writer already belongs to a different native tree handle".into(),
                );
            }
            return Ok(owner);
        }
        store::preflight(&native.native_authority, store::MAX_AUTHORITY_BYTES)
            .map_err(|e| e.to_string())?;
        let path = native
            .persist_path
            .clone()
            .ok_or("native owner requires the actual state-file path")?;
        native
            .native_runtime
            .bind(&path, &native.native_authority)
            .map_err(|e| e.to_string())?;
        if native.native_authority.pending.len() > store::MAX_PENDING {
            return Err("native pending enrollment quota exceeded".into());
        }
        let mut pending_slots = BTreeSet::new();
        for pending in native.native_authority.pending.values() {
            if !pending_slots.insert(pending.target.workspace) {
                return Err("ambiguous native pending enrollment slot".into());
            }
        }
        let mut current = Current::default();
        current
            .required
            .extend(native.native_authority.rooms.keys().copied());
        current
            .required
            .extend(native.native_authority.pending.keys().copied());
        for (room, pending) in &native.native_authority.pending {
            enrollment::validate_pending(*room, pending)?;
        }
        store::preflight(&*native, store::MAX_STATE_BYTES).map_err(|e| e.to_string())?;
        let mut migrated = native.clone();
        let mut migration_changed = false;
        for (room, record) in &native.native_authority.rooms {
            let mut view = enrollment::restore(*room, record, SystemTime::now())?;
            migration_changed |= publication::migrate(&mut view)?;
            migrated
                .native_authority
                .rooms
                .insert(*room, view.record.clone());
            current.rooms.insert(*room, Arc::new(view));
        }
        if migration_changed {
            migrated.native_authority.revision = migrated
                .native_authority
                .revision
                .checked_add(1)
                .ok_or("native storage generation exhausted")?;
        }
        let owner = Self(Arc::new(SharedOwner {
            tree: tree.clone(),
            runtime: native.native_runtime.clone(),
            mutation: Mutex::new(()),
            current: RwLock::new(current),
            listeners: Mutex::new(Vec::new()),
        }));
        // Re-establish the publication barrier before exposing a reopened generation.
        let bytes = persist::encode_tree(&migrated).map_err(|e| e.to_string())?;
        native
            .native_runtime
            .publish(
                &path,
                &native.native_authority,
                &migrated.native_authority,
                &bytes,
            )
            .map_err(|e| e.to_string())?;
        *native = migrated;
        for (room, view) in &owner.0.current.read().rooms {
            native.project_native_room(*room, &view.peer);
        }
        native.native_runtime.0.writer.lock().owner = Arc::downgrade(&owner.0);
        native.export_fs_capabilities();
        Ok(owner)
    }
    pub(crate) fn contains_room(&self, room: RoomId) -> bool {
        self.0.current.read().required.contains(&room) || self.0.runtime.requires_uncertain(room)
    }
    /// Store availability only; room authority still requires `with_current`.
    pub(crate) fn health(&self) -> Result<(), String> {
        if self.0.current.read().fenced || !self.0.runtime.healthy() {
            return Err("native owner continuity is fenced".into());
        }
        Ok(())
    }
    pub(crate) fn with_current<T>(
        &self,
        room: RoomId,
        read: impl FnOnce(&NativeCommittedView) -> Result<T, String>,
    ) -> Result<T, String> {
        let current = self.0.current.read();
        if current.fenced || current.denied.contains(&room) || !self.0.runtime.healthy() {
            return Err("native owner continuity is fenced".into());
        }
        read(
            current
                .rooms
                .get(&room)
                .ok_or("room has no committed native enrollment")?,
        )
    }
    pub(crate) fn observe_fences(
        &self,
        observer: Arc<dyn NativeFenceObserver>,
    ) -> NativeFenceSubscription {
        self.0.listeners.lock().push(Arc::downgrade(&observer));
        NativeFenceSubscription {
            _observer: observer,
        }
    }
    pub(crate) fn invalidate_identity(&self, room: RoomId) {
        let mut current = self.0.current.write();
        if !current.denied.insert(room) {
            return;
        }
        let continuity = current.rooms.get(&room).map_or(0, |view| view.continuity());
        self.0.notify(room, continuity);
    }
    /// Admits genuine relay provenance; pending document prerequisites are also durable.
    pub(crate) fn receive(
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
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        let report = candidate
            .peer
            .receive_from_report(origin, message, now)
            .map_err(|e| e.to_string())?;
        publication::retire(&mut candidate, now)?;
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_room(room, candidate, None)?;
        Ok(report)
    }
    /// Caller holds the serialized turn, but readers keep the preceding committed view during I/O.
    fn publish_room(
        &self,
        room: RoomId,
        mut candidate: NativeCommittedView,
        enrollment: Option<&StoredPendingEnrollment>,
    ) -> Result<(), String> {
        let candidate_security = candidate.security_digest()?;
        let changed = {
            let current = self.0.current.read();
            if current.fenced || current.denied.contains(&room) || !self.0.runtime.healthy() {
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
        store::preflight(&candidate.record, store::MAX_AUTHORITY_BYTES)
            .map_err(|e| e.to_string())?;
        if changed {
            candidate.record.continuity = candidate
                .record
                .continuity
                .checked_add(1)
                .ok_or("native continuity exhausted")?;
        }
        let mut tree = self.0.tree.write();
        store::preflight(&*tree, store::MAX_STATE_BYTES).map_err(|e| e.to_string())?;
        if let Some(pending) = enrollment {
            if tree.room_of(pending.target.workspace) != Some(pending.target.prior_room)
                || tree
                    .native_authority
                    .pending
                    .get(&room)
                    .is_none_or(|current| {
                        current.id != pending.id
                            || current.status != PendingEnrollmentStatus::Active
                    })
            {
                return Err(
                    "enrollment target or pending generation changed before completion".into(),
                );
            }
        }
        let mut next = tree.clone();
        if let Some(pending) = enrollment {
            // This pure helper cannot reach any persistence/export/publisher channel.
            next.set_room_in_memory(pending.target.workspace, room);
            if next.room_of(pending.target.workspace) != Some(room) {
                return Err("enrollment local slot cannot be bound".into());
            }
        }
        next.native_authority.revision = next
            .native_authority
            .revision
            .checked_add(1)
            .ok_or("native storage generation exhausted")?;
        next.native_authority
            .rooms
            .insert(room, candidate.record.clone());
        if enrollment.is_some() {
            next.native_authority.pending.remove(&room);
        }
        next.project_native_room(room, &candidate.peer);
        let bytes = persist::encode_tree(&next).map_err(|e| e.to_string())?;
        let path = tree
            .persist_path
            .as_ref()
            .ok_or("native owner state-file path removed")?;
        tree.native_runtime
            .publish(path, &tree.native_authority, &next.native_authority, &bytes)
            .map_err(|e| e.to_string())?;
        let mut current = self.0.current.write();
        if changed {
            self.0.notify(room, candidate.continuity());
        }
        current.required.insert(room);
        current.rooms.insert(room, Arc::new(candidate));
        *tree = next;
        drop(current);
        // Only the acknowledged final admitted projection reaches authority exports.
        tree.export_fs_capabilities();
        Ok(())
    }
}
