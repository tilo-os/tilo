//! A device's workspaces: the admitted state of every workspace the device takes
//! part in ([`NativeCommittedView`]), and the durable record of it
//! ([`NativeAuthorityStorage`]). Every change is admitted into a candidate,
//! made durable through the device's [`Store`] and only then installed, so no
//! reader ever sees state the disk may not hold.
//!
//! [`crate::enroll`] redeems an invitation with the device that issued it, and
//! [`session`] holds the link to the device the workspace is kept on.
mod answers;
mod compact;
#[cfg(not(target_arch = "wasm32"))]
mod file_store;
#[cfg(test)]
#[path = "owner/fixture_tests.rs"]
pub(crate) mod fixture;
mod history_content;
mod history_keys;
mod journal;
mod journaled;
pub(crate) mod limits;
mod publication;
pub mod renewal;
#[cfg(not(target_arch = "wasm32"))]
pub mod session;
pub(crate) mod stored;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
#[cfg(test)]
mod tests;
pub(crate) mod write;

pub use crate::certified::{VerifiedNativeEnrollment, VerifiedNativeEnrollmentIntent};
pub use crate::document_read::{
    CommittedDocument, DocumentChanges, DocumentRead, History, Holding, revision_of_heads,
};
pub use crate::draft::{LocalWrite, admit_local_write, draft_local_write};
pub use crate::enroll;
pub(crate) use crate::enrollment;
#[cfg(not(target_arch = "wasm32"))]
pub use file_store::{FileHost, open_file, open_file_with, owner_file_of, read_file};
#[cfg(not(target_arch = "wasm32"))]
pub use history_content::HistoryContent;
pub use history_keys::SealedHistoryKeys;
#[cfg(not(target_arch = "wasm32"))]
pub use journaled::{JournalParts, Journaled};
pub use publication::{ownership_name_between, ownership_run_name};
pub use stored::{
    NativeAuthorityStorage, PendingEnrollmentId, PendingEnrollmentStatus, StoredPendingEnrollment,
};
pub use write::{DocumentWrites, WorkspaceDocumentWrite, WriteRefusal};

use crate::replica::Replica;
use crate::{AdmissionReport, StatePeer, TransportOrigin};
use neighbour_wire::NeighbourMsg;
use parking_lot::{Mutex, MutexGuard, RwLock};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Weak},
};
use tilo_desktop_model::WorkspaceId;
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

/// Tell this process's authorization watchers that something they read may
/// have changed. A page has none: it decides nothing for other processes.
fn policy_changed() {
    #[cfg(not(target_arch = "wasm32"))]
    tilo_identity::policy_change::announce();
}

/// Whether `hint` names this device, `device`, or the workspace service it
/// certified, as the workspace's holder: a workspace placed on this device,
/// whose service is in this process. Such a workspace is joined over the
/// service's in-process carrier, never dialled: an endpoint cannot dial
/// itself.
#[must_use]
pub fn hosted_by(hint: &crate::join::TransportHint, device: &[u8; 32]) -> bool {
    hint.host.is_of_device(device)
}

/// Check `hint` as the address of `device`'s own host of `workspace_id`: the
/// host signed it, and it names the workspace it keeps and the device that
/// signed it.
pub(crate) fn validate_host_hint(
    hint: &crate::join::TransportHint,
    workspace_id: WorkspaceId,
    device: &DeviceCert,
) -> Result<(), String> {
    if !hosted_by(hint, &device.device_public_key) {
        return Err("the workspace's host hint is not this device's own".into());
    }
    let (payload, _) =
        tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1::authenticate_transport_hint(
            hint,
        )
        .map_err(|error| error.to_string())?;
    if payload.workspace_id != workspace_id {
        return Err("the workspace's host hint names another workspace".into());
    }
    if payload.device_certificate != hint.host {
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
    /// Durably keep `bytes`, a run of `workspace`'s outgoing ownership waiting behind its head,
    /// beside the record, and answer the hash it is named by. The store keeps it while the
    /// record names it.
    fn put_ownership(&self, _workspace: WorkspaceId, _bytes: &[u8]) -> Result<[u8; 32], String> {
        Err("native outgoing ownership runs are unavailable".into())
    }
    /// Read a run of ownership [`Self::put_ownership`] kept.
    fn read_ownership(&self, _workspace: WorkspaceId, _hash: [u8; 32]) -> Result<Vec<u8>, String> {
        Err("native outgoing ownership runs are unavailable".into())
    }
    /// False once a publication may or may not have reached disk; the owner
    /// then stays fenced until it is reopened from what the disk holds.
    fn healthy(&self) -> bool;
    /// Whether such an uncertain publication may have made `workspace` durable.
    fn uncertain(&self, workspace: WorkspaceId) -> bool;
    /// Whether the host's shell holds `workspace`.
    fn shows(&self, workspace: WorkspaceId) -> bool;
    /// Make `next` durable, then call `install` before anyone can observe
    /// the new record. Nothing is installed when this fails.
    pub(crate) fn publish(
        &self,
        next: &NativeAuthorityStorage,
        change: Change,
        install: &mut dyn FnMut(),
    ) -> Result<(), String>;
}

/// What a publication changes besides the record itself.
pub enum Change {
    /// Only pending-enrollment bookkeeping.
    Pending,
    /// `workspace` now holds what the record being published holds; a
    /// completed enrollment installs into the workspace made for it, which is
    /// `workspace` already.
    Workspace { workspace: WorkspaceId, shown: bool },
    /// This device no longer holds `workspace`: its replica and any enrollment
    /// into it are gone from the record, and the host takes the workspace's
    /// shared desktop off in the same publication. Nobody else's data goes: the
    /// workspace lives on with its governor and members.
    Released { workspace: WorkspaceId },
}

/// Why a completed enrollment was not installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallError {
    /// The host's Complete or the governance it carries was refused: nothing
    /// was installed, and the enrollment is no longer worth retrying.
    Refused(String),
    /// This device could not admit what the host sent (an isolated Automerge
    /// worker crashed, overran its deadline or never started). Nothing says
    /// the enrollment is wrong: it stays pending for the next attempt.
    Device(String),
}
impl InstallError {
    /// Whether this device failed, rather than the admission refusing.
    #[must_use]
    pub fn is_device_failure(&self) -> bool {
        matches!(self, Self::Device(_))
    }
}
impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(reason) | Self::Device(reason) => f.write_str(reason),
        }
    }
}
impl std::error::Error for InstallError {}
impl From<String> for InstallError {
    fn from(reason: String) -> Self {
        Self::Refused(reason)
    }
}
impl From<&str> for InstallError {
    fn from(reason: &str) -> Self {
        Self::Refused(reason.into())
    }
}
impl From<crate::AdmissionError> for InstallError {
    fn from(error: crate::AdmissionError) -> Self {
        if error.is_device_failure() {
            Self::Device(error.to_string())
        } else {
            Self::Refused(error.to_string())
        }
    }
}

#[derive(Clone)]
pub struct NativeWorkspaceOwner(pub(crate) Arc<SharedOwner>);
/// Does not keep the owner alive; a host's writer holds one to fence it.
#[derive(Clone)]
pub struct WeakNativeWorkspaceOwner(Weak<SharedOwner>);
struct SharedOwner {
    pub(crate) store: Arc<dyn Store>,
    /// The committed record; replaced only inside a publication's `install`.
    pub(crate) authority: RwLock<NativeAuthorityStorage>,
    pub(crate) mutation: Mutex<()>,
    pub(crate) current: RwLock<Current>,
    listeners: Mutex<Vec<Weak<dyn NativeFenceObserver>>>,
    /// The channel keys that keep this device's sealed record histories within
    /// their bound ([`NativeWorkspaceOwner::keep_sealed_histories`]).
    history_keys: Mutex<Option<Arc<dyn SealedHistoryKeys>>>,
}
#[derive(Default)]
struct Current {
    pub(crate) workspaces: BTreeMap<WorkspaceId, Arc<NativeCommittedView>>,
    pub(crate) required: BTreeSet<WorkspaceId>,
    pub(crate) denied: BTreeSet<WorkspaceId>,
    pub(crate) fenced: bool,
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
    pub(crate) peer: StatePeer,
    /// This device's live replica, shared with the workspace's direct links.
    replica: Replica,
    record: stored::StoredWorkspace,
    /// Who holds the workspace's keys, as [`Self::key_holders`] last
    /// answered and for which instant.
    holders: Arc<std::sync::Mutex<Option<KeyHolders>>>,
    /// What the view's publication changes for the workspace's other devices
    /// ([`security_digest`]), taken once: it reads every member's identity,
    /// and the next publication compares against it.
    security: Arc<std::sync::OnceLock<[u8; 32]>>,
}
/// The devices holding a workspace's keys, and the instant (ns) they were
/// asked for.
type KeyHolders = (u64, std::collections::BTreeSet<[u8; 32]>);
impl NativeCommittedView {
    fn new(peer: StatePeer, record: stored::StoredWorkspace) -> Self {
        Self {
            replica: Replica::new(peer.clone()),
            peer,
            record,
            holders: Arc::default(),
            security: Arc::default(),
        }
    }
    /// The devices that hold the workspace's keys at `now` (as its
    /// governance's `key_holders` decides it), once for each instant: a page
    /// asks again for every frame its group takes in, thousands while it
    /// arrives in a workspace of hundreds of people, and the answer cannot
    /// change while the view and the instant stay the same.
    ///
    /// # Errors
    /// The view's governance cannot be read.
    pub fn key_holders(
        &self,
        now: web_time::SystemTime,
    ) -> Result<std::collections::BTreeSet<[u8; 32]>, String> {
        use tilo_lock::MutexRecover as _;
        let at_ns = crate::state::now_ns(now);
        if let Some((at, holders)) = &*self.holders.lock_recover()
            && *at == at_ns
        {
            return Ok(holders.clone());
        }
        let holders = self.peer.governance().key_holders(now)?;
        *self.holders.lock_recover() = Some((at_ns, holders.clone()));
        Ok(holders)
    }
    /// The devices of `people` among [`Self::key_holders`] at `now`: who holds
    /// the keys of a group only those people read
    /// (as its governance's `reader_key_holders` decides it).
    ///
    /// # Errors
    /// The view's governance cannot be read.
    pub fn reader_key_holders(
        &self,
        people: &std::collections::BTreeSet<[u8; 32]>,
        now: web_time::SystemTime,
    ) -> Result<std::collections::BTreeSet<[u8; 32]>, String> {
        self.peer.governance().reader_key_holders(people, now)
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
        change: impl FnOnce(&mut StatePeer, &mut stored::StoredWorkspace) -> Result<T, String>,
    ) -> Result<(T, stored::StoredWorkspace), String> {
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
    /// Whether this device created the workspace, itself or through the
    /// workspace service it certified, and so founds its MLS group: that
    /// service takes part in the group with this device's keys.
    #[must_use]
    pub fn founded_here(&self) -> bool {
        let Some(founder) = self.peer.governance().founder() else {
            return false;
        };
        let device = &self.device().device_public_key;
        // The founder's certificate writes for this device: it is this device's, or that of the
        // workspace service this device certified.
        founder == *device
            || self
                .peer
                .certificates()
                .iter()
                .any(|cert| cert.device_public_key == founder && cert.is_of_device(device))
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
    fn enrollment(&self) -> Result<&stored::StoredEnrollment, String> {
        self.record
            .enrollment
            .as_ref()
            .ok_or_else(|| "the workspace's record holds no enrollment".into())
    }
    #[must_use]
    pub fn device(&self) -> &DeviceCert {
        &self.record.device
    }
    /// The workspace this device's record holds.
    #[must_use]
    pub fn workspace_id(&self) -> WorkspaceId {
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
    /// Where the devices keeping this workspace are reached, each its own
    /// signed address, by device key: the holders the workspace's governance
    /// names, and, for a workspace its governance names none of yet (one
    /// founded here before its host named itself), the hint this device keeps
    /// for it. A client tries every holder, so none of them comes first.
    #[must_use]
    pub fn keeping_devices(&self) -> Vec<crate::join::TransportHint> {
        let named = self.peer.governance().keeping_devices();
        if !named.is_empty() {
            return named;
        }
        self.transport_hint().ok().cloned().into_iter().collect()
    }
    /// Where the host this device's own entry names is reached: the hint its
    /// enrollment carries, the one the host's own answer named for a
    /// workspace this device holds as a further device of its person, or, for
    /// a workspace founded here, the one its own host recorded as it founded
    /// it. [`Self::keeping_devices`] is where the workspace's holders are
    /// reached now that a workspace has several.
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
    /// ([`StatePeer::same_policy`]) and the record's access in the workspace.
    fn same_policy(&self, peer: &StatePeer, record: &stored::StoredWorkspace) -> bool {
        self.peer.same_policy(peer) && self.record.same_access(record)
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
/// device of the workspace: its security basis and its signer. A publication that leaves it as it was is no change the workspace's
/// devices have to hear about.
fn security_digest(peer: &StatePeer, device: &DeviceCert) -> Result<[u8; 32], String> {
    let basis = peer
        .security_basis()
        .transpose()
        .map_err(|e| e.to_string())?;
    let bytes = postcard::to_stdvec(&(basis, device)).map_err(|e| e.to_string())?;
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
    ///
    /// `policy` says whether the publication changes what authorization
    /// decides on: the workspaces held, their governance, certificates and
    /// revocations, or this device's access in them. Its watchers re-check
    /// on that, not on a write to an app record, which wakes the reads of
    /// that record itself.
    pub(crate) fn publish(
        &self,
        next: &NativeAuthorityStorage,
        change: Change,
        policy: bool,
        install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        let result = self.store.publish(next, change, install);
        if result.is_err() && !self.store.healthy() {
            self.fence_all();
        }
        if result.is_ok() {
            crate::work::commit();
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
            current: RwLock::new(Current::default()),
            listeners: Mutex::new(Vec::new()),
            history_keys: Mutex::new(None),
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
            current.workspaces.insert(*workspace, Arc::new(view));
        }
        Ok(Self(Arc::new(SharedOwner {
            store,
            authority: RwLock::new(authority),
            mutation: Mutex::new(()),
            current: RwLock::new(current),
            listeners: Mutex::new(Vec::new()),
            history_keys: Mutex::new(None),
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
    /// publication ([`Change::Released`]). The workspace itself, its governor and its other members are
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

    /// Let `written`, what a store's journal of `workspace` goes on from, go on
    /// from the parts the restored replica encodes itself in, so its next
    /// entry carries what that write changed. `false` when the workspace is
    /// not held here or its replica encodes other bytes: the store writes a
    /// snapshot first then.
    pub(crate) fn resume_journal(
        &self,
        workspace: WorkspaceId,
        written: &mut journal::Written,
    ) -> bool {
        self.with_current(workspace, |view| view.peer().durable_layout())
            .is_ok_and(|parts| written.resume(parts))
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
        let mut record = view.record.clone();
        let durable = {
            let live = view.live();
            stored::DurablePeer::from(live.durable_layout()?)
        };
        if durable == record.peer {
            return Ok(());
        }
        record.peer = durable;
        self.publish(workspace, &view.replica, record, None)
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
    /// Admits genuine server provenance; pending document prerequisites are also durable.
    pub fn receive(
        &self,
        workspace: WorkspaceId,
        origin: TransportOrigin,
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<AdmissionReport, crate::AdmissionError> {
        self.receive_batch(workspace, vec![(origin, message)], now)
            .map_err(crate::AdmissionError::malformed)?
            .pop()
            .ok_or_else(|| "native owner admitted no input".to_owned())
            .map_err(crate::AdmissionError::malformed)?
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
        now: SystemTime,
    ) -> Result<Vec<Result<AdmissionReport, crate::AdmissionError>>, String> {
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        let replica = candidate.replica.clone();
        let mut reports = Vec::with_capacity(inputs.len());
        for (origin, message) in inputs {
            let report = self.admit_with_keys(&candidate, origin, message, now);
            reports.push(report);
        }
        if reports.iter().all(Result::is_err) {
            // Every input was refused: nothing changed to commit.
            return Ok(reports);
        }
        // One snapshot under the replica's lock: the durable bytes that show
        // whether anything changed are the ones published, encoded once.
        let mut record = candidate.record.clone();
        let snapshot = {
            let live = replica.lock();
            let durable = stored::DurablePeer::from(live.durable_layout()?);
            // Authentication and causal admission still run for every replay.
            // Reuse the actual committed barrier only when the durable peer
            // state is byte-for-byte unchanged: newly settled prerequisites
            // still require publication before they can be observed.
            if durable == record.peer {
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
    /// [`Self::admit_input`], with a compaction the replica refuses decided once more with the
    /// keys this device holds ([`Self::receive_behind`]).
    fn admit_with_keys(
        &self,
        candidate: &NativeCommittedView,
        origin: TransportOrigin,
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<AdmissionReport, crate::AdmissionError> {
        Self::admit_input(candidate, origin, message, now)
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
    ) -> Result<AdmissionReport, crate::AdmissionError> {
        let workspace = candidate.workspace_id();
        let mut live = candidate.live();
        if matches!(&message, NeighbourMsg::AppDocument { .. })
            && !matches!(origin, TransportOrigin::RetainedNativePublication { .. })
        {
            if let Some(exact) = live.lookup_exact_document_report(origin, &message, now)? {
                return Ok(exact);
            }
        }
        let report = live.receive_from_report(origin, message, now)?;
        // A message let go for newer ones is no loss: its server retains it, and it is asked for
        // again once what it waited for arrives. While a replica waits for its governance, every
        // write of a busy app past the few it holds is let go so, which said one by one buried
        // the log; only one refused or too large to hold is worth a warning.
        for dropped in &report.summary().dropped {
            if matches!(dropped.reason, crate::DropReason::Superseded) {
                tracing::debug!(%workspace, %dropped, "a held app message was let go, to be asked for again");
            } else {
                tracing::warn!(%workspace, %dropped, "a held app message was let go unapplied");
            }
        }
        Ok(report)
    }
    /// Publish `workspace` as `candidate` holds it: the record's peer bytes and
    /// the reader's snapshot both come from `replica`, the live copy.
    ///
    /// # Errors
    /// The owner is fenced, the record exceeds its bounds, or the publication
    /// could not be made durable.
    pub(crate) fn publish_workspace(
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

    /// Publish `record` for `workspace`, leaving this device's live replica as
    /// it is: a change of the record alone.
    fn publish_document(
        &self,
        workspace: WorkspaceId,
        view: &NativeCommittedView,
        record: stored::StoredWorkspace,
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
        mut record: stored::StoredWorkspace,
        enrollment: Option<&StoredPendingEnrollment>,
    ) -> Result<(), String> {
        // One snapshot, taken under the replica's lock: what is published is
        // this device's copy exactly as it was made durable, never a frame a
        // link admitted after it.
        let snapshot = {
            let live = replica.lock();
            record.peer = live.durable_layout()?.into();
            Self::snapshot(&live, &mut record)?
        };
        self.install_snapshot(workspace, replica, record, snapshot, enrollment)
    }

    /// The replica as the record publishes it.
    fn snapshot(
        live: &StatePeer,
        _record: &mut stored::StoredWorkspace,
    ) -> Result<StatePeer, String> {
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
        mut record: stored::StoredWorkspace,
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
            holders: Arc::default(),
            security,
        });
        let continuity = candidate.continuity();
        self.0.publish(
            &next,
            Change::Workspace {
                workspace,
                shown: enrollment.is_some_and(|pending| pending.shown),
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
        // evidence is gone (folded by a compaction),
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
    pub document_id: String,
    /// The author's access in the workspace.
    pub author: tilo_action_policy::MemberAccess,
    /// Whether the host replayed it from what it retains.
    pub replayed: bool,
    pub envelope: collab_types::SignedOp,
}
