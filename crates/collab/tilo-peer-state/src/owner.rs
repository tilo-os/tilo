//! A device's workspaces: replicas of workspaces a remote durable host governs, and
//! workspaces this device governs itself.
//!
//! [`NativeWorkspaceOwner`] holds the admitted state of every workspace this
//! device enrolled in or created ([`NativeCommittedView`]) and is the only writer of
//! their durable record, [`NativeAuthorityStorage`]. Every change is admitted
//! into a candidate, published through the host's [`Store`] and only then
//! installed, so no reader or receipt ever sees state the disk may not hold.
//! A host keeps that record beside its state file: an index naming each
//! workspace's file, and one file per workspace ([`open_file`]).
//! [`enroll`] redeems an invitation with the host that issued it, and
//! [`session`] holds the relay co-edit session with that host.
mod certified;
mod checkpoint;
mod create;
mod document_read;
mod effect_history;
mod effect_receipt;
#[cfg(not(target_arch = "wasm32"))]
pub mod enroll;
mod enrollment;
#[cfg(not(target_arch = "wasm32"))]
mod file_store;
#[cfg(test)]
#[path = "owner/fixture_tests.rs"]
pub(crate) mod fixture;
#[cfg(not(target_arch = "wasm32"))]
mod history_content;
#[cfg(not(target_arch = "wasm32"))]
mod join_service;
mod limits;
mod publication;
mod record;
pub mod renewal;
#[cfg(not(target_arch = "wasm32"))]
pub mod session;
mod shortcuts;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
#[cfg(test)]
mod tests;
mod vouched;
mod workspace_membership;
mod write;

pub use certified::{VerifiedNativeEnrollment, VerifiedNativeEnrollmentIntent};
pub use checkpoint::{
    CheckpointAcknowledgement, CheckpointCapture, CheckpointCompleteness, CheckpointEntry,
    CheckpointRoute, CheckpointSchema, MaterializedCheckpoint, NameBinding, NativeCheckpoint,
};
pub use document_read::{
    CommittedDocument, DocumentChanges, DocumentRead, History, Holding, revision_of_heads,
};
#[cfg(not(target_arch = "wasm32"))]
pub use file_store::{FileHost, open_file, open_file_with, owner_file_of, read_file};
#[cfg(not(target_arch = "wasm32"))]
pub use history_content::HistoryContent;
#[cfg(not(target_arch = "wasm32"))]
pub use join_service::OwnerJoinWorkspaces;
pub use limits::{LimitedWriter, MAX_AUTHORITY_BYTES, MAX_STATE_BYTES, preflight};
pub use publication::{
    CUSTODY_FULL, HostAnswer, NativeOutgoing, NativeOutgoingEntry, RecordWrites,
    validate_file_layout,
};
pub use record::{
    EnrollmentTarget, NativeAuthorityStorage, PendingEnrollmentId, PendingEnrollmentStatus,
    StoredPendingEnrollment,
};
pub use write::{Committed, Write};

use crate::replica::Replica;
use crate::{AdmissionReport, StatePeer, TransportOrigin};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use parking_lot::{Mutex, MutexGuard, RwLock};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Weak},
};
use tilo_desktop_model::{WorkspaceId, WorkspaceSlot};
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

/// Tell this process's authorization watchers that something they read may
/// have changed. A page has none: it decides nothing for other processes.
fn policy_changed() {
    #[cfg(not(target_arch = "wasm32"))]
    tilo_identity_host::policy_change::announce();
}

/// Whether `hint` names this device, `device`, as the workspace's host: a workspace
/// placed on this device, whose host runs in this process. Such a workspace is
/// joined over the host's in-process carrier, never dialled: an endpoint
/// cannot dial itself.
#[must_use]
pub fn hosted_by(hint: &crate::join::TransportHint, device: &[u8; 32]) -> bool {
    &hint.host.device_public_key == device
}

/// Check `hint` as the address of `device`'s own host of `room`, the
/// workspace `workspace_id`: the host signed it, and it names the workspace it
/// keeps and the device that signed it.
pub(crate) fn validate_host_hint(
    hint: &crate::join::TransportHint,
    room: &str,
    workspace_id: u64,
    device: &DeviceCert,
) -> Result<(), String> {
    if !hosted_by(hint, &device.device_public_key) {
        return Err("the workspace's host hint is not this device's own".into());
    }
    let (payload, _) = tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1::
        authenticate_transport_hint(hint)
        .map_err(|error| error.to_string())?;
    if payload.room != room || payload.workspace_id != workspace_id {
        return Err("the workspace's host hint names another workspace".into());
    }
    if payload.host_device_certificate != *device {
        return Err("the workspace's host hint is not the host's own signed address".into());
    }
    Ok(())
}

/// Where an owner's record is durable. A page's store lives on its one
/// thread, so only a device with threads shares it between them.
pub trait Store: std::any::Any + collab_types::signer::MaybeSendSync {
    /// Durably retain immutable owner history through the existing content backend.
    /// The state publication may reference the returned address only after this succeeds.
    fn put_history(&self, _bytes: &[u8]) -> Result<[u8; 32], String> {
        Err("native owner history content is unavailable".into())
    }
    /// Read and verify a retained history block. Absence is an error, not UUID absence.
    fn read_history(&self, _hash: [u8; 32]) -> Result<Vec<u8>, String> {
        Err("native owner history content is unavailable".into())
    }
    /// False once a publication may or may not have reached disk; the owner
    /// then stays fenced until it is reopened from what the disk holds.
    fn healthy(&self) -> bool;
    /// Whether such an uncertain publication may have made `workspace` durable.
    fn uncertain(&self, workspace: WorkspaceId) -> bool;
    /// The workspace a local slot is, where the host has slots.
    fn slot_workspace(&self, slot: WorkspaceSlot) -> Option<WorkspaceId>;
    /// Make `next` durable, then call `install` before anyone can observe
    /// the new record. Nothing is installed when this fails.
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
    /// `workspace` now holds what the record being published holds; a
    /// completed enrollment installs into the workspace made for it, which is
    /// `workspace` already.
    Workspace {
        workspace: WorkspaceId,
        into: Option<&'a EnrollmentTarget>,
    },
    /// This device no longer holds `workspace`: its replica and any enrollment
    /// into it are gone from the record, and the host takes the workspace's
    /// shared desktop off in the same publication. Nobody else's data goes: the
    /// workspace lives on with its governor and members.
    Released { workspace: WorkspaceId },
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
    /// One write of this device at a time, from its draft to its commit
    /// ([`NativeWorkspaceOwner::write`]).
    writes: tokio::sync::Mutex<()>,
    current: RwLock<Current>,
    listeners: Mutex<Vec<Weak<dyn NativeFenceObserver>>>,
    /// Who hands on what this device commits to a workspace it hosts itself
    /// ([`NativeWorkspaceOwner::observe_kept_here`]).
    kept_here: Mutex<Vec<Weak<dyn NativeKeptHereObserver>>>,
}
#[derive(Default)]
struct Current {
    workspaces: BTreeMap<WorkspaceId, Arc<NativeCommittedView>>,
    required: BTreeSet<WorkspaceId>,
    denied: BTreeSet<WorkspaceId>,
    fenced: bool,
}
/// Closure-scoped borrowed view of the actual owner publication. It is never an authority DTO.
///
/// The view carries two things: the replica as this device holds it now, which a
/// direct link admits into, and that replica as the last published record made
/// it durable, which is what every reader decides on. Only a publication moves
/// one into the other.
#[derive(Clone)]
pub struct NativeCommittedView {
    /// The replica as the last publication made it durable.
    peer: StatePeer,
    /// This device's live replica, shared with the workspace's direct links.
    replica: Replica,
    record: record::StoredWorkspace,
    /// Who reads each channel, as [`Self::channel_readers`] and
    /// [`Self::unit_scope`] last answered.
    readers: Arc<std::sync::Mutex<ChannelReaders>>,
    /// What the view's publication changes for the workspace's other devices
    /// ([`security_digest`]), taken once: it reads every member's identity,
    /// and the next publication compares against it.
    security: Arc<std::sync::OnceLock<[u8; 32]>>,
}
/// A channel asked about, and the route of its record when the asker knows it.
type ChannelAsked = (collab_types::ChannelId, Option<collab_types::ChannelRoute>);
/// Who reads the channels and records asked about at one instant, as a view's
/// governance decides it: the view never changes, so neither do they.
#[derive(Default)]
struct ChannelReaders {
    at_ns: u64,
    readers: std::collections::HashMap<ChannelAsked, Option<std::collections::BTreeSet<[u8; 32]>>>,
    scopes: std::collections::HashMap<collab_types::ChannelRoute, collab_types::ChannelId>,
}
impl ChannelReaders {
    /// The answers held for `at_ns`, dropping those of any other instant.
    fn at(&mut self, at_ns: u64) -> &mut Self {
        if self.at_ns != at_ns {
            self.readers.clear();
            self.scopes.clear();
            self.at_ns = at_ns;
        }
        self
    }
}
impl NativeCommittedView {
    fn new(peer: StatePeer, record: record::StoredWorkspace) -> Self {
        Self {
            replica: Replica::new(peer.clone()),
            peer,
            record,
            readers: Arc::default(),
            security: Arc::default(),
        }
    }
    /// The devices that may read `channel` at `now`
    /// (as its governance's `channel_readers` decides it), once for each
    /// channel and instant: a page asks again for every frame its channels
    /// take in, thousands while it arrives in a workspace of hundreds of
    /// people, and the answer cannot change while the view and the instant
    /// stay the same.
    ///
    /// # Errors
    /// The view's governance cannot be read.
    pub fn channel_readers(
        &self,
        now: web_time::SystemTime,
        channel: collab_types::ChannelId,
        route: Option<&collab_types::ChannelRoute>,
    ) -> Result<Option<std::collections::BTreeSet<[u8; 32]>>, String> {
        use tilo_lock::MutexRecover as _;
        let at_ns = crate::state::now_ns(now);
        let key = (channel, route.cloned());
        if let Some(answer) = self.readers.lock_recover().at(at_ns).readers.get(&key) {
            return Ok(answer.clone());
        }
        let answer = self
            .peer
            .governance()
            .channel_readers(now, channel, route)?;
        self.readers
            .lock_recover()
            .at(at_ns)
            .readers
            .insert(key, answer.clone());
        Ok(answer)
    }
    /// The key scope `route`'s record seals under at `now` (as its
    /// governance's `unit_scope` decides it), once for each record and
    /// instant, as [`Self::channel_readers`] is.
    ///
    /// # Errors
    /// The view's governance cannot be read.
    pub fn unit_scope(
        &self,
        now: web_time::SystemTime,
        route: &collab_types::ChannelRoute,
    ) -> Result<collab_types::ChannelId, String> {
        use tilo_lock::MutexRecover as _;
        let at_ns = crate::state::now_ns(now);
        if let Some(scope) = self.readers.lock_recover().at(at_ns).scopes.get(route) {
            return Ok(*scope);
        }
        let scope = self.peer.governance().unit_scope(now, route)?;
        self.readers
            .lock_recover()
            .at(at_ns)
            .scopes
            .insert(route.clone(), scope);
        Ok(scope)
    }
    /// The replica as the record published it.
    #[must_use]
    pub fn peer(&self) -> &StatePeer {
        &self.peer
    }
    /// This device's live replica: what its links admit into, and what a change
    /// of this device's own copy is made in.
    #[must_use]
    pub fn replica(&self) -> &Replica {
        &self.replica
    }
    /// This device's live replica, held: a caller that reads and changes it as
    /// one step, where a link cannot admit in between.
    fn live(&self) -> MutexGuard<'_, StatePeer> {
        self.replica.lock()
    }
    /// Make one of this device's own changes: `change` sees a copy of the live
    /// replica and of the record, and only what it answers is installed, so a
    /// refusal changes nothing and no admission of a link interleaves.
    fn change<T>(
        &self,
        change: impl FnOnce(&mut StatePeer, &mut record::StoredWorkspace) -> Result<T, String>,
    ) -> Result<(T, record::StoredWorkspace), String> {
        let mut live = self.live();
        let mut next = live.clone();
        let mut record = self.record.clone();
        let answer = change(&mut next, &mut record)?;
        *live = next;
        drop(live);
        Ok((answer, record))
    }
    /// The person who owns the workspace: their root key.
    #[must_use]
    pub fn owner(&self) -> [u8; 32] {
        self.peer.owner()
    }
    /// The host the workspace is pinned to: the device whose host orders the
    /// workspace's commits.
    #[must_use]
    pub fn workspace_host(&self) -> &DeviceCert {
        &self.record.workspace_host
    }
    /// Whether this device's own host orders the workspace's commits.
    #[must_use]
    pub fn hosted_here(&self) -> bool {
        self.record.hosted_here()
    }
    /// Whether this device entered a workspace that existed before it: it
    /// redeemed an invitation, or a host vouched for it as a further device of
    /// its person. What the workspace holds reaches it from the workspace's
    /// host.
    #[must_use]
    pub fn entered(&self) -> bool {
        self.record.enrollment.is_some() || self.record.vouched.is_some()
    }
    /// The signed invitation this device enrolled with.
    pub fn enrollment_invitation(&self) -> Result<&[u8], String> {
        Ok(&self.enrollment()?.invitation)
    }
    fn enrollment(&self) -> Result<&record::StoredEnrollment, String> {
        self.record
            .enrollment
            .as_ref()
            .ok_or_else(|| "the workspace's record holds no enrollment".into())
    }
    #[must_use]
    pub fn device(&self) -> &DeviceCert {
        &self.record.device
    }
    /// The workspace's id in this device's record: the partition its changes
    /// live in, as the relay names it.
    #[must_use]
    pub fn workspace_id(&self) -> u64 {
        self.record.workspace_id
    }
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.record.generation
    }
    #[must_use]
    pub fn continuity(&self) -> u64 {
        self.record.continuity
    }
    /// Where the host that governs this workspace is reached: the hint its
    /// enrollment carries, the one the host's own answer named for a workspace
    /// this device holds as a further device of its person, or, for a
    /// workspace founded here, the one its own host recorded as it founded it.
    pub fn transport_hint(&self) -> Result<&crate::join::TransportHint, String> {
        self.record
            .enrollment
            .as_ref()
            .map(|enrollment| &enrollment.transport_hint)
            .or_else(|| self.record.vouched.as_ref()?.hint.as_ref())
            .or(self.record.host_hint.as_ref())
            .ok_or_else(|| "the workspace's record holds no host hint".to_owned())
    }
    /// Whether a publication of `peer` and `record` leaves everything
    /// authorization reads from this view as it is: the replica's governance
    /// ([`StatePeer::same_policy`]) and the record's standing in the workspace.
    fn same_policy(&self, peer: &StatePeer, record: &record::StoredWorkspace) -> bool {
        self.peer.same_policy(peer) && self.record.same_standing(record)
    }
    fn security_digest(&self) -> Result<[u8; 32], String> {
        if let Some(digest) = self.security.get() {
            return Ok(*digest);
        }
        let digest = security_digest(&self.peer, &self.record.device)?;
        Ok(*self.security.get_or_init(|| digest))
    }
}

/// What a publication of `peer` under `device` would change for every other
/// device of the workspace: its media governance, its security basis and its
/// signer. A publication that leaves it as it was is no change the workspace's
/// devices have to hear about.
fn security_digest(peer: &StatePeer, device: &DeviceCert) -> Result<[u8; 32], String> {
    let policy = peer.validated_media_policy().ok().flatten();
    let basis = peer
        .security_basis()
        .transpose()
        .map_err(|e| e.to_string())?;
    let bytes = postcard::to_stdvec(&(policy, basis, device)).map_err(|e| e.to_string())?;
    Ok(*blake3::hash(&bytes).as_bytes())
}

/// Callbacks synchronously revoke live client/capture fences; no owner reread or I/O.
pub trait NativeFenceObserver: Send + Sync {
    fn fence(&self, workspace: WorkspaceId, continuity: u64);
}
impl<F: Fn(WorkspaceId, u64) + Send + Sync> NativeFenceObserver for F {
    fn fence(&self, workspace: WorkspaceId, continuity: u64) {
        self(workspace, continuity);
    }
}
/// Retaining this token retains the callback; owner stores only a weak reference.
pub struct NativeFenceSubscription {
    _observer: Arc<dyn NativeFenceObserver>,
}

/// Told what this device committed to a workspace it hosts itself, once per
/// commit, after it is durable: such a workspace keeps no outgoing custody
/// (its own device is its keeper) and holds no link to a host, so what its
/// device writes reaches the members linked to it only through whoever is
/// told here.
pub trait NativeKeptHereObserver: Send + Sync {
    fn kept_here(&self, workspace: WorkspaceId, message: &NeighbourMsg);
}
impl<F: Fn(WorkspaceId, &NeighbourMsg) + Send + Sync> NativeKeptHereObserver for F {
    fn kept_here(&self, workspace: WorkspaceId, message: &NeighbourMsg) {
        self(workspace, message);
    }
}
/// Retaining this token retains the observer; owner stores only a weak reference.
pub struct NativeKeptHereSubscription {
    _observer: Arc<dyn NativeKeptHereObserver>,
}
impl SharedOwner {
    /// Tell [`NativeKeptHereObserver`]s what this device committed to
    /// `workspace`, which it hosts itself.
    fn kept_here(&self, workspace: WorkspaceId, message: &NeighbourMsg) {
        let observers: Vec<_> = {
            let mut observers = self.kept_here.lock();
            let live = observers.iter().filter_map(Weak::upgrade).collect();
            observers.retain(|observer| observer.strong_count() != 0);
            live
        };
        for observer in observers {
            observer.kept_here(workspace, message);
        }
    }
    fn notify(&self, workspace: WorkspaceId, continuity: u64) {
        let observers: Vec<_> = {
            let mut listeners = self.listeners.lock();
            let live = listeners.iter().filter_map(Weak::upgrade).collect();
            listeners.retain(|listener| listener.strong_count() != 0);
            live
        };
        for observer in observers {
            observer.fence(workspace, continuity);
        }
    }
    fn fence_all(&self) {
        let mut current = self.current.write();
        if current.fenced {
            return;
        }
        current.fenced = true;
        for (workspace, view) in &current.workspaces {
            self.notify(*workspace, view.continuity());
        }
        drop(current);
        policy_changed();
    }
    fn fenced(&self, current: &Current, workspace: WorkspaceId) -> bool {
        current.fenced || current.denied.contains(&workspace) || !self.store.healthy()
    }
    /// Publish through the store; an uncertain failure fences every workspace.
    ///
    /// `policy` says whether the publication changes what authorization
    /// decides on: the workspaces held, their governance, certificates and
    /// revocations, or this device's standing in them. Its watchers re-check
    /// on that, not on a write to an app record, which wakes the reads of
    /// that record itself.
    fn publish(
        &self,
        next: &NativeAuthorityStorage,
        change: Change<'_>,
        policy: bool,
        install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        let result = self.store.publish(next, change, install);
        if result.is_err() && !self.store.healthy() {
            self.fence_all();
        }
        if policy && result.is_ok() {
            policy_changed();
        }
        result
    }
}
impl Drop for SharedOwner {
    fn drop(&mut self) {
        // Workspaces that required this owner have none now.
        policy_changed();
    }
}
impl WeakNativeWorkspaceOwner {
    #[must_use]
    pub fn upgrade(&self) -> Option<NativeWorkspaceOwner> {
        self.0.upgrade().map(NativeWorkspaceOwner)
    }
}
impl NativeWorkspaceOwner {
    /// An owner of no workspaces yet, publishing through `store`.
    #[must_use]
    pub fn empty(store: Arc<dyn Store>) -> Self {
        Self(Arc::new(SharedOwner {
            store,
            authority: RwLock::new(NativeAuthorityStorage::default()),
            mutation: Mutex::new(()),
            writes: tokio::sync::Mutex::new(()),
            current: RwLock::new(Current::default()),
            listeners: Mutex::new(Vec::new()),
            kept_here: Mutex::new(Vec::new()),
        }))
    }
    /// Restore every enrolled workspace of `authority`, the record `store` last
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
        current
            .required
            .extend(authority.workspaces.keys().copied());
        current.required.extend(authority.pending.keys().copied());
        for (workspace, pending) in &authority.pending {
            enrollment::validate_pending(*workspace, pending)?;
        }
        for (workspace, record) in &authority.workspaces {
            let view = enrollment::restore(*workspace, record, now)?;
            effect_receipt::validate_history(&view, store.as_ref())?;
            checkpoint::validate_restore(&view, store.as_ref())?;
            current.workspaces.insert(*workspace, Arc::new(view));
        }
        Ok(Self(Arc::new(SharedOwner {
            store,
            authority: RwLock::new(authority),
            mutation: Mutex::new(()),
            writes: tokio::sync::Mutex::new(()),
            current: RwLock::new(current),
            listeners: Mutex::new(Vec::new()),
            kept_here: Mutex::new(Vec::new()),
        })))
    }
    #[must_use]
    pub fn downgrade(&self) -> WeakNativeWorkspaceOwner {
        WeakNativeWorkspaceOwner(Arc::downgrade(&self.0))
    }
    /// Fence every workspace: the host's store lost certainty about the disk.
    pub fn fence_all(&self) {
        self.0.fence_all();
    }
    /// Stop holding `workspace` here: drop this device's replica of it and any
    /// enrollment into it, with the host's workspace of it in the same
    /// publication ([`Change::Released`]). A workspace and its slot go
    /// together; the workspace itself, its governor and its other members are
    /// untouched. Answers whether the record held the workspace.
    pub fn release_workspace(&self, workspace: WorkspaceId) -> Result<bool, String> {
        let _turn = self.0.mutation.lock();
        let mut next = self.0.authority.read().clone();
        let held = next.workspaces.remove(&workspace).is_some();
        let joining = next.pending.remove(&workspace).is_some();
        if !held && !joining {
            return Ok(false);
        }
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or("native storage generation exhausted")?;
        let mut continuity = 0;
        self.0
            .publish(&next, Change::Released { workspace }, true, &mut || {
                {
                    let mut current = self.0.current.write();
                    continuity = current
                        .workspaces
                        .remove(&workspace)
                        .map_or(0, |view| view.continuity());
                    current.required.remove(&workspace);
                    current.denied.remove(&workspace);
                }
                *self.0.authority.write() = next.clone();
            })?;
        // Every session of the workspace ends: its replica is gone. Told once the
        // publication is over, with no lock of the host's held.
        self.0.notify(workspace, continuity);
        Ok(true)
    }
    /// The record as last published.
    #[must_use]
    pub fn storage(&self) -> NativeAuthorityStorage {
        self.0.authority.read().clone()
    }
    /// The revision of the record as last published: every publication
    /// raises it.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.0.authority.read().revision
    }
    /// Every workspace with a committed enrollment.
    #[must_use]
    pub fn workspaces(&self) -> Vec<WorkspaceId> {
        self.0.current.read().workspaces.keys().copied().collect()
    }
    #[must_use]
    pub fn contains_workspace(&self, workspace: WorkspaceId) -> bool {
        self.0.current.read().required.contains(&workspace) || self.0.store.uncertain(workspace)
    }

    /// The workspaces this device hosts itself, in one read of the record: its
    /// own host orders their records, whether or not the record says where
    /// that host is reached. What names its own address in them reads this
    /// ([`NativeWorkspaceOwner::name_host`]): most workspaces a device holds
    /// are another device's to host, and their records are not copied to find
    /// that out.
    #[must_use]
    pub fn hosted(&self) -> Vec<WorkspaceId> {
        let current = self.0.current.read();
        current
            .workspaces
            .iter()
            .filter(|(_, view)| view.record.hosted_here())
            .map(|(workspace, _)| *workspace)
            .collect()
    }

    /// Store availability only; workspace authority still requires `with_current`.
    pub fn health(&self) -> Result<(), String> {
        if self.0.current.read().fenced || !self.0.store.healthy() {
            return Err("native owner continuity is fenced".into());
        }
        Ok(())
    }
    pub fn with_current<T>(
        &self,
        workspace: WorkspaceId,
        read: impl FnOnce(&NativeCommittedView) -> Result<T, String>,
    ) -> Result<T, String> {
        read(&*self.view(workspace)?)
    }

    /// This device's live replica of `workspace`: what its direct link admits
    /// into, and what [`Self::persist`] publishes.
    ///
    /// # Errors
    /// The workspace is not held here, or the owner is fenced.
    pub fn replica(&self, workspace: WorkspaceId) -> Result<Replica, String> {
        Ok(self.view(workspace)?.replica.clone())
    }

    /// The workspace's committed view, fenced workspaces included in the check.
    fn view(&self, workspace: WorkspaceId) -> Result<Arc<NativeCommittedView>, String> {
        let current = self.0.current.read();
        if self.0.fenced(&current, workspace) {
            return Err("native owner continuity is fenced".into());
        }
        current
            .workspaces
            .get(&workspace)
            .cloned()
            .ok_or_else(|| "workspace has no committed native enrollment".to_owned())
    }

    /// Publish `workspace`'s live replica: what its links and this device
    /// admitted into it becomes durable, and every reader sees it. Nothing is
    /// published when nothing changed since the last publication.
    ///
    /// # Errors
    /// The workspace is not held here, it is fenced, or the publication could
    /// not be made durable.
    pub fn persist(&self, workspace: WorkspaceId) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        self.persist_locked(workspace)
    }

    fn persist_locked(&self, workspace: WorkspaceId) -> Result<(), String> {
        let view = self.view(workspace)?;
        let previous = view.record.publication.clone();
        let mut record = view.record.clone();
        let durable = {
            let live = view.live();
            publication::refresh_governance_checkpoint(&live, &mut record)?;
            live.durable()?
        };
        if durable == record.peer && record.publication == previous {
            return Ok(());
        }
        self.publish(workspace, &view.replica, record, None)
    }

    /// Publish `workspace`'s live replica whenever `changes` says a link
    /// admitted into it, until the link is gone.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn persist_changes(
        &self,
        workspace: WorkspaceId,
        changes: &mut tokio::sync::watch::Receiver<u64>,
    ) {
        while changes.changed().await.is_ok() {
            if let Err(error) = self.persist(workspace) {
                tracing::warn!(%error, ws = %workspace, "a workspace's admitted state was not published");
            }
        }
    }
    /// The verdicts this device recorded on writes in `workspace`, each with
    /// the changes of the write it refused: what its author reads to see where
    /// its write is not. Nothing outside this device's own slice is returned:
    /// another device's verdict is that device's to show.
    #[must_use]
    pub fn write_refusals(&self, workspace: WorkspaceId) -> Vec<neighbour_wire::WriteRefusal> {
        self.with_current(workspace, |view| {
            Ok(view.peer().workspace().write_refusals())
        })
        .unwrap_or_default()
    }

    /// The name each device the workspace's roster certifies gave itself, by
    /// its key: what a person reads where a refusal says a device's name.
    pub fn device_labels(&self, workspace: WorkspaceId) -> BTreeMap<[u8; 32], String> {
        self.with_current(workspace, |view| {
            Ok(view
                .peer()
                .certificates()
                .into_iter()
                .map(|certificate| (certificate.device_public_key, certificate.device_label))
                .collect())
        })
        .unwrap_or_default()
    }

    /// Be told what this device commits to a workspace it hosts itself, for as long as the
    /// returned token is kept.
    pub fn observe_kept_here(
        &self,
        observer: Arc<dyn NativeKeptHereObserver>,
    ) -> NativeKeptHereSubscription {
        self.0.kept_here.lock().push(Arc::downgrade(&observer));
        NativeKeptHereSubscription {
            _observer: observer,
        }
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
    pub fn invalidate_identity(&self, workspace: WorkspaceId) {
        let mut current = self.0.current.write();
        if !current.denied.insert(workspace) {
            return;
        }
        let continuity = current
            .workspaces
            .get(&workspace)
            .map_or(0, |view| view.continuity());
        self.0.notify(workspace, continuity);
        drop(current);
        policy_changed();
    }
    /// Admits genuine relay provenance; pending document prerequisites are also durable.
    pub fn receive(
        &self,
        workspace: WorkspaceId,
        origin: TransportOrigin,
        message: NeighbourMsg,
        key: Option<&SigningKey>,
        now: SystemTime,
    ) -> Result<AdmissionReport, String> {
        self.receive_batch(workspace, vec![(origin, message)], key, now)?
            .pop()
            .ok_or_else(|| "native owner admitted no input".to_owned())?
    }
    /// Admit `inputs`, in order, into `workspace` and commit the workspace once for
    /// all of them: a host's replay of a busy workspace is thousands of inputs,
    /// and committing the workspace for each one rewrote the whole state file
    /// thousands of times. Each input is decided on its own, exactly as
    /// [`Self::receive`] decides it, and its refusal changes nothing; the
    /// answer is one result per input. Nothing of the batch is observable,
    /// and no report is returned, before the one commit is durable: when
    /// it fails, every input fails with it.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the admission and the record it retires are decided under one replica turn"
    )]
    pub fn receive_batch(
        &self,
        workspace: WorkspaceId,
        inputs: Vec<(TransportOrigin, NeighbourMsg)>,
        key: Option<&SigningKey>,
        now: SystemTime,
    ) -> Result<Vec<Result<AdmissionReport, String>>, String> {
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        let replica = candidate.replica.clone();
        let previous_publication = candidate.record.publication.clone();
        let device = candidate.device().clone();
        let mut reports = Vec::with_capacity(inputs.len());
        // The workspace changes admitted from its other devices, as they came:
        // their hashes are read only to retire changes of this device's own
        // ([`publication::retire_member_changes`]), which a device that
        // published none (a page entering a workspace) never has.
        let mut governed: Vec<Vec<Vec<u8>>> = Vec::new();
        let mut verdicts = false;
        for (origin, message) in inputs {
            let received = Self::received_changes(origin, &message).cloned();
            let report = Self::admit_input(&candidate, origin, message.clone(), now);
            if report.is_ok()
                && let Some(changes) = received
            {
                governed.push(changes);
            }
            // A refused write reaches its author: this device's verdict on it
            // is kept in its own slice of the workspace, which the next sync
            // carries back, and an admitted write clears the verdict it lands
            // on. One carrier, the workspace document itself.
            if let Some(key) = key
                && matches!(message, NeighbourMsg::AppDocument { .. })
            {
                let mut live = replica.lock();
                let verdict = match &report {
                    Ok(_) => live
                        .workspace_mut()
                        .clear_landed_write(&device, &message, key, now),
                    Err(reason) => live
                        .workspace_mut()
                        .refuse_write(&device, &message, reason, key, now),
                };
                match verdict {
                    Ok(filed) => verdicts |= filed,
                    Err(error) => {
                        tracing::warn!(%error, "a document write's verdict was not kept");
                    }
                }
            }
            reports.push(report);
        }
        if reports.iter().all(Result::is_err) && !verdicts {
            // Every input was refused and every verdict was already kept:
            // nothing changed to commit.
            return Ok(reports);
        }
        let mut record = candidate.record.clone();
        // One snapshot under the replica's lock, as [`Self::publish`] takes
        // it: the durable bytes that show whether anything changed are the
        // ones published, encoded once.
        let snapshot = {
            let live = replica.lock();
            publication::retire(&live, &mut record)?;
            publication::retire_member_changes(&live, &mut record, &governed)?;
            publication::refresh_governance_checkpoint(&live, &mut record)?;
            let durable = live.durable()?;
            // Authentication and causal admission still run for every replay.
            // Reuse the actual committed barrier only when both durable peer
            // state and outgoing custody are byte-for-byte unchanged. In
            // particular, newly settled prerequisites or retired obligations
            // still require publication before they can be observed.
            if durable == record.peer && record.publication == previous_publication {
                return Ok(reports);
            }
            record.peer = durable;
            record.generation = record
                .generation
                .checked_add(1)
                .ok_or("native owner generation exhausted")?;
            Self::snapshot(&live, &mut record)?
        };
        self.install_snapshot(workspace, &replica, record, snapshot, None)?;
        Ok(reports)
    }
    /// The workspace changes `message` carries, from any device of the workspace: each
    /// participant admits every other's, and the workspace's governance that now
    /// carries what this device published retires it
    /// ([`publication::retire_member_changes`]). A delta that does not
    /// carry them all retires nothing.
    fn received_changes(origin: TransportOrigin, message: &NeighbourMsg) -> Option<&Vec<Vec<u8>>> {
        if !matches!(
            origin,
            TransportOrigin::RelayDelta(_) | TransportOrigin::RetainedRelayDelta(_)
        ) {
            return None;
        }
        match message {
            NeighbourMsg::SyncResp { changes, .. } => Some(changes),
            _ => None,
        }
    }
    /// Decide one input into the workspace's live replica: refused, it changes nothing.
    ///
    /// A workspace's devices send each other more than its state over the same
    /// link: a member's ask or answer about a record's content rides it too,
    /// and the replica decides that as a family it keeps none of
    /// ([`crate::InputDisposition::UnsupportedFamily`]), the same answer a
    /// device syncing the replica alone gives it. So nothing is turned away
    /// here for not being state.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the whole admission is one turn on the replica"
    )]
    fn admit_input(
        candidate: &NativeCommittedView,
        origin: TransportOrigin,
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<AdmissionReport, String> {
        let workspace = candidate.workspace_id();
        let mut live = candidate.live();
        if matches!(&message, NeighbourMsg::AppDocument { .. })
            && !matches!(origin, TransportOrigin::RetainedNativePublication { .. })
        {
            let exact = live
                .lookup_exact_document_report(origin, &message, now)
                .map_err(|error| error.to_string())?;
            if let Some(exact) = exact
                && publication::exact_replay_unchanged(&live, &candidate.record)?
            {
                return Ok(exact);
            }
        }
        let report = live
            .receive_from_report(origin, message, now)
            .map_err(|e| e.to_string())?;
        for dropped in &report.summary().dropped {
            tracing::warn!(%workspace, %dropped, "a held app message was let go unapplied");
        }
        Ok(report)
    }
    /// Publish `workspace` as `candidate` holds it: the record's peer bytes and
    /// the reader's snapshot both come from `replica`, the live copy.
    ///
    /// # Errors
    /// The owner is fenced, the record exceeds its bounds, or the publication
    /// could not be made durable.
    fn publish_workspace(
        &self,
        workspace: WorkspaceId,
        candidate: NativeCommittedView,
        enrollment: Option<&StoredPendingEnrollment>,
    ) -> Result<(), String> {
        let NativeCommittedView {
            replica, record, ..
        } = candidate;
        self.publish(workspace, &replica, record, enrollment)
    }

    /// Publish `workspace`'s record as `change` leaves a copy of it, keeping
    /// this device's live replica: a change of this device's record alone.
    #[cfg(not(target_arch = "wasm32"))]
    fn change_record<T>(
        &self,
        workspace: WorkspaceId,
        change: impl FnOnce(&NativeCommittedView, &mut record::StoredWorkspace) -> Result<T, String>,
    ) -> Result<T, String> {
        let view = self.view(workspace)?;
        let mut record = view.record.clone();
        let answer = change(&view, &mut record)?;
        self.publish_record(workspace, &view, record, None)?;
        Ok(answer)
    }

    /// Publish `record` for `workspace`, leaving this device's live replica as
    /// it is: a change of the record alone.
    fn publish_record(
        &self,
        workspace: WorkspaceId,
        view: &NativeCommittedView,
        record: record::StoredWorkspace,
        enrollment: Option<&StoredPendingEnrollment>,
    ) -> Result<(), String> {
        self.publish(workspace, &view.replica, record, enrollment)
    }

    /// Caller holds the serialized turn, but readers keep the preceding committed view during I/O.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "fence observers are notified in order under the continuity write lock"
    )]
    fn publish(
        &self,
        workspace: WorkspaceId,
        replica: &Replica,
        mut record: record::StoredWorkspace,
        enrollment: Option<&StoredPendingEnrollment>,
    ) -> Result<(), String> {
        // One snapshot, taken under the replica's lock: what is published is
        // this device's copy exactly as it was made durable, never a frame a
        // link admitted after it.
        let snapshot = {
            let live = replica.lock();
            publication::refresh_governance_checkpoint(&live, &mut record)?;
            record.peer = live.durable()?;
            Self::snapshot(&live, &mut record)?
        };
        self.install_snapshot(workspace, replica, record, snapshot, enrollment)
    }

    /// The rest of a publication's snapshot of `live`, whose durable bytes
    /// `record` holds: the effect history root, and the replica as published.
    fn snapshot(
        live: &StatePeer,
        record: &mut record::StoredWorkspace,
    ) -> Result<StatePeer, String> {
        // The signed root is the replica's, and the store's blocks follow
        // it: the record mirrors it rather than deciding it, so a
        // publication that did not reach disk still converges on the next.
        if live.admitted_workspace().is_some() {
            record.effect_history_root = live
                .workspace()
                .workspace_effect_history_root(&record.device)?;
        }
        Ok(live.clone())
    }

    /// Make `record` and the replica `snapshot` took durable and current.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "fence observers are notified in order under the continuity write lock"
    )]
    fn install_snapshot(
        &self,
        workspace: WorkspaceId,
        replica: &Replica,
        mut record: record::StoredWorkspace,
        published: StatePeer,
        enrollment: Option<&StoredPendingEnrollment>,
    ) -> Result<(), String> {
        // What the publication changes for the workspace's other devices
        // ([`security_digest`]), taken when a comparison needs it: it reads
        // every member, and a replica taking in its workspace's governance
        // for the first time changes it whatever the governance says.
        let security = Arc::new(std::sync::OnceLock::new());
        // And whether it changes what authorization decides on: most
        // publications carry an app record's write and nothing else.
        let (changed, policy) = {
            let current = self.0.current.read();
            if self.0.fenced(&current, workspace) {
                return Err("native owner continuity is fenced".into());
            }
            match current.workspaces.get(&workspace) {
                None => (true, true),
                Some(old)
                    if old.peer.admitted_workspace().is_some()
                        != published.admitted_workspace().is_some() =>
                {
                    (true, true)
                }
                Some(old) => {
                    let digest = security_digest(&published, &record.device)?;
                    let changed = old.security_digest()? != *security.get_or_init(|| digest);
                    (changed, changed || !old.same_policy(&published, &record))
                }
            }
        };
        limits::preflight(&record, limits::MAX_AUTHORITY_BYTES).map_err(|e| e.to_string())?;
        if changed {
            record.continuity = record
                .continuity
                .checked_add(1)
                .ok_or("native continuity exhausted")?;
        }
        let mut next = self.0.authority.read().clone();
        if let Some(pending) = enrollment
            && next.pending.get(&workspace).is_none_or(|current| {
                current.id != pending.id || current.status != PendingEnrollmentStatus::Active
            })
        {
            return Err("enrollment target or pending generation changed before completion".into());
        }
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or("native storage generation exhausted")?;
        next.workspaces.insert(workspace, record.clone());
        if enrollment.is_some() {
            next.pending.remove(&workspace);
        }
        let candidate = Arc::new(NativeCommittedView {
            replica: replica.clone(),
            peer: published,
            record,
            readers: Arc::default(),
            security,
        });
        let continuity = candidate.continuity();
        self.0.publish(
            &next,
            Change::Workspace {
                workspace,
                into: enrollment.and_then(|pending| pending.target.as_ref()),
            },
            policy,
            &mut || {
                let mut current = self.0.current.write();
                if changed {
                    self.0.notify(workspace, continuity);
                }
                current.required.insert(workspace);
                current.workspaces.insert(workspace, Arc::clone(&candidate));
                *self.0.authority.write() = next.clone();
            },
        )
    }
}

/// Whether the workspace took a frame in, as a device's apps read it: a write
/// it accepted or holds already, this device's own included.
#[must_use]
pub fn took_in(report: &crate::AdmissionReport, retained: bool) -> bool {
    match report.primary() {
        crate::InputDisposition::Accepted(_) | crate::InputDisposition::Duplicate(_) => true,
        // A host's replay of a frame this device took in before: its exact
        // evidence is gone (a legacy state, or one folded by a compaction),
        // so admission reports it as history it already holds. This device's
        // apps may be new — a restarted session keeps the workspace and
        // starts its apps empty — and the frame is part of their record: it
        // goes to them as any other replayed frame does, and their CRDT drops
        // what they already hold.
        crate::InputDisposition::RetryLater(
            crate::MissingPrerequisite::ExactHistoricalEvidence,
        ) => retained && report.duplicate,
        _ => false,
    }
}

/// An app write the owner took in for this device's apps: another device's, or this device's own.
#[derive(Clone)]
pub struct AppWrite {
    /// The writer's peer handle, as the write names it.
    pub peer: u64,
    pub app_id: String,
    pub cluster_id: String,
    pub record_id: String,
    /// The author's standing in the workspace.
    pub author: tilo_action_policy::Standing,
    /// Whether the host replayed it from what it retains.
    pub replayed: bool,
    pub envelope: collab_types::SignedOp,
}
