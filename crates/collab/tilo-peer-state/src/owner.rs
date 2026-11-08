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
mod application_record;
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

pub use application_record::{ApplicationInitialization, ApplicationRecord};
pub use certified::{VerifiedNativeEnrollment, VerifiedNativeEnrollmentIntent};
pub use checkpoint::{
    CheckpointAcknowledgement, CheckpointCapture, CheckpointCompleteness, CheckpointEntry,
    CheckpointRoute, CheckpointSchema, MaterializedCheckpoint, NameBinding, NativeCheckpoint,
};
pub use document_read::{ChangesRead, CommittedDocument, DocumentChanges, DocumentRead};
#[cfg(not(target_arch = "wasm32"))]
pub use file_store::{FileHost, open_file, open_file_with, owner_file_of, read_file};
#[cfg(not(target_arch = "wasm32"))]
pub use history_content::HistoryContent;
#[cfg(not(target_arch = "wasm32"))]
pub use join_service::OwnerJoinWorkspaces;
pub use limits::{LimitedWriter, MAX_AUTHORITY_BYTES, MAX_STATE_BYTES, preflight};
pub use publication::{NativeOutgoing, NativeOutgoingEntry, validate_file_layout};
pub use record::{
    EnrollmentTarget, NativeAuthorityStorage, PendingEnrollmentId, PendingEnrollmentStatus,
    StoredPendingEnrollment,
};

use crate::{AdmissionReport, StatePeer, TransportOrigin};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use parking_lot::{Mutex, RwLock};
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
    /// `workspace` now holds `peer`; a completed enrollment installs into the
    /// workspace made for it, which is `workspace` already.
    Workspace {
        workspace: WorkspaceId,
        peer: &'a StatePeer,
        /// The host the workspace's commits are ordered by.
        host: &'a DeviceCert,
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
    current: RwLock<Current>,
    listeners: Mutex<Vec<Weak<dyn NativeFenceObserver>>>,
}
#[derive(Default)]
struct Current {
    workspaces: BTreeMap<WorkspaceId, Arc<NativeCommittedView>>,
    required: BTreeSet<WorkspaceId>,
    denied: BTreeSet<WorkspaceId>,
    fenced: bool,
}
/// Closure-scoped borrowed view of the actual owner publication. It is never an authority DTO.
#[derive(Clone)]
pub struct NativeCommittedView {
    peer: StatePeer,
    record: record::StoredWorkspace,
}
impl NativeCommittedView {
    #[must_use]
    pub fn peer(&self) -> &StatePeer {
        &self.peer
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
        self.record.device == self.record.workspace_host
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
    fn security_digest(&self) -> Result<[u8; 32], String> {
        let policy = self.peer.validated_media_policy().ok().flatten();
        let basis = self
            .peer
            .admitted_workspace()
            .map(|doc| crate::workspace::media::security_basis(doc, &self.peer.revocations()))
            .transpose()
            .map_err(|e| e.to_string())?;
        let bytes = postcard::to_stdvec(&(policy, basis, &self.record.device))
            .map_err(|e| e.to_string())?;
        Ok(*blake3::hash(&bytes).as_bytes())
    }
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
impl SharedOwner {
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
        // A new publication can change a workspace's tasks, which the broker
        // offers as actions: it re-checks on this rather than on a clock.
        policy_changed();
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
            current: RwLock::new(Current::default()),
            listeners: Mutex::new(Vec::new()),
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
            current: RwLock::new(current),
            listeners: Mutex::new(Vec::new()),
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
            .publish(&next, Change::Released { workspace }, &mut || {
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

    /// The workspaces this device hosts whose record does not say where their
    /// host is reached, in one read of the record: the ones founded here
    /// before the record kept its own host's hint, which no device reaches a
    /// host for until it does.
    #[must_use]
    pub fn hosts_unnamed(&self) -> Vec<WorkspaceId> {
        let current = self.0.current.read();
        current
            .workspaces
            .iter()
            .filter(|(_, view)| {
                view.record.device == view.record.workspace_host && !view.record.names_host()
            })
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
        let current = self.0.current.read();
        if self.0.fenced(&current, workspace) {
            return Err("native owner continuity is fenced".into());
        }
        read(
            current
                .workspaces
                .get(&workspace)
                .ok_or("workspace has no committed native enrollment")?,
        )
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
    pub fn receive_batch(
        &self,
        workspace: WorkspaceId,
        inputs: Vec<(TransportOrigin, NeighbourMsg)>,
        key: Option<&SigningKey>,
        now: SystemTime,
    ) -> Result<Vec<Result<AdmissionReport, String>>, String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(workspace, |view| Ok(view.clone()))?;
        let previous_publication = candidate.record.publication.clone();
        let device = candidate.device().clone();
        let mut reports = Vec::with_capacity(inputs.len());
        let mut governed = BTreeSet::new();
        let mut verdicts = false;
        for (origin, message) in inputs {
            let received = Self::received_changes(origin, &message);
            let report = Self::admit_input(&mut candidate, workspace, origin, message.clone(), now);
            if report.is_ok()
                && let Some(changes) = received
            {
                governed.extend(changes);
            }
            // A refused write reaches its author: this device's verdict on it
            // is kept in its own slice of the workspace, which the next sync
            // carries back, and an admitted write clears the verdict it lands
            // on. One carrier, the workspace document itself.
            if let Some(key) = key
                && matches!(message, NeighbourMsg::AppDocument { .. })
            {
                let verdict = match &report {
                    Ok(_) => candidate
                        .peer
                        .workspace_mut()
                        .clear_landed_write(&device, &message, key, now),
                    Err(reason) => candidate
                        .peer
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
        publication::retire(&mut candidate)?;
        publication::retire_member_changes(&mut candidate, &governed)?;
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
        self.publish_workspace(workspace, candidate, None)?;
        Ok(reports)
    }
    /// The workspace changes `message` carries, from any device of the workspace: each
    /// participant admits every other's, and the workspace's governance that now
    /// carries what this device published retires it
    /// ([`publication::retire_member_changes`]). A delta that does not
    /// carry them all retires nothing.
    fn received_changes(
        origin: TransportOrigin,
        message: &NeighbourMsg,
    ) -> Option<BTreeSet<automerge::ChangeHash>> {
        if !matches!(
            origin,
            TransportOrigin::RelayDelta(_) | TransportOrigin::RetainedRelayDelta(_)
        ) {
            return None;
        }
        if !matches!(message, NeighbourMsg::SyncResp { .. }) {
            return None;
        }
        publication::change_hashes(match message {
            NeighbourMsg::SyncResp { changes, .. } => changes,
            _ => return None,
        })
        .ok()
    }
    /// Decide one relay input into `candidate`: refused, it changes nothing.
    fn admit_input(
        candidate: &mut NativeCommittedView,
        workspace: WorkspaceId,
        origin: TransportOrigin,
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<AdmissionReport, String> {
        if !matches!(
            message,
            NeighbourMsg::SyncResp { .. }
                | NeighbourMsg::AppDocument { .. }
                | NeighbourMsg::IdentityHello { .. }
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
            tracing::warn!(%workspace, %dropped, "a held app message was let go unapplied");
        }
        Ok(report)
    }
    /// Caller holds the serialized turn, but readers keep the preceding committed view during I/O.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "fence observers are notified in order under the continuity write lock"
    )]
    fn publish_workspace(
        &self,
        workspace: WorkspaceId,
        mut candidate: NativeCommittedView,
        enrollment: Option<&StoredPendingEnrollment>,
    ) -> Result<(), String> {
        publication::refresh_governance_checkpoint(&mut candidate)?;
        let candidate_security = candidate.security_digest()?;
        let changed = {
            let current = self.0.current.read();
            if self.0.fenced(&current, workspace) {
                return Err("native owner continuity is fenced".into());
            }
            current
                .workspaces
                .get(&workspace)
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
        next.workspaces.insert(workspace, candidate.record.clone());
        if enrollment.is_some() {
            next.pending.remove(&workspace);
        }
        let candidate = Arc::new(candidate);
        self.0.publish(
            &next,
            Change::Workspace {
                workspace,
                peer: &candidate.peer,
                host: candidate.workspace_host(),
                into: enrollment.and_then(|pending| pending.target.as_ref()),
            },
            &mut || {
                let mut current = self.0.current.write();
                if changed {
                    self.0.notify(workspace, candidate.continuity());
                }
                current.required.insert(workspace);
                current.workspaces.insert(workspace, Arc::clone(&candidate));
                *self.0.authority.write() = next.clone();
            },
        )
    }
}
