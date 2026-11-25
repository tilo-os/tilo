//! Certified workspace and document replica. Transport identity is always an input.
use crate::admission::{
    AdmissionError, AdmissionReport, DocumentAdmission, DroppedInput, ExactAdmissions,
    InputDisposition, MissingPrerequisite,
};
use crate::cached_document::CachedDocument;
use crate::verified_certificate::VerifiedCertificate;
use crate::{NeighbourMsg, workspace};
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use collab_types::{
    ArtifactKind, DocumentMutation, Ed25519PublicKey, InboundState, OpKind, Person, Role, SignedOp,
};
#[cfg(test)]
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use tilo_action_policy::{
    ActionKind, Grant, GrantId, Grantee, MemberAccess, TreePath, WorkspacePolicy,
};
use tilo_lock::MutexRecover;
use tilo_user_identity::{DeviceCert, RevocationList};
pub use tilo_workspace_join::{DocumentRow, Member, MemberId, MemberList};
use web_time::{Duration, SystemTime};
mod action_parts;
#[cfg(test)]
mod action_parts_tests;
mod backend;
mod document_capture;
mod documents;
pub use document_capture::{
    Captured, KeptDocument, PeerDocumentCapture, PeerDocumentReadError, changes_since,
    committed_document_revision, saved_whole,
};
#[cfg(test)]
pub(crate) mod json_resource_tests;
use crate::shared::Shared;
use backend::{PeerCore, PeerDocument};
mod bans;
mod covered_documents;
mod member_documents;
use member_documents::{empty_genesis, empty_genesis_op};
mod facades;
mod governance;
pub use governance::GovernanceHeads;
mod governance_writes;
pub use facades::{
    PeerGovernance, PeerGovernanceMut, PeerOutbox, PeerOutboxMut, PeerPublication,
    PeerPublicationMut, PeerWorkspace, PeerWorkspaceMut,
};
pub use governance_writes::ActionStamp;
pub(crate) use governance_writes::{commit_governance, wall_secs};
mod app_versions;
mod calls;
pub use app_versions::InstalledApp;
pub use calls::CallGate;
pub(crate) mod package_registries;
pub(crate) mod package_trust;
pub use package_registries::{CHANNELS, PackageRegistry};
pub use package_trust::{RevokedPublisher, TrustAttestor};
pub(crate) mod certificate_renewal;
mod copies;
mod delegates;
mod device_rows;
pub use copies::{
    COPY_POLICY_KEY, CopyPolicy, CopyRole, CopyStatus, HELD_COPIES_KEY, HeldCopy, HolderCopy,
    Missing, Shortfall,
};
mod leases;
pub use leases::{CLOCK_SKEW_NS, LEASES_KEY, Lease, LeaseClaim, unranked, vm_lease, vm_rank};
mod schedules;
pub use schedules::{SCHEDULES_KEY, trigger_mark};
mod done;
pub use done::{DONE_KEY, Done, mail_send_lease};
#[cfg(test)]
mod schedules_tests;
#[cfg(test)]
pub(crate) mod test_desktops;
pub use delegates::{ActingAs, DelegateInactive, GovernedAgent, GovernedDelegateRow};
mod local_admission;
pub(crate) use document_capture::OpenedRefusal;
pub(crate) use local_admission::DraftRefusal;
pub use local_admission::NotAdmitted;
mod local_outbox;
mod membership_error;
mod owner_membership;
pub use membership_error::MembershipError;
mod workspace_delta;
mod workspace_effects;
mod workspace_membership;
mod workspace_refusals;
pub use workspace_refusals::ChangeRefused;
mod workspace_shortcuts;
pub use local_outbox::Reseal;
mod governed_coverage;
mod held;
mod joined;
mod publication;
mod workspace_grants;
pub use workspace_grants::workspace_grants_of;
mod imported_history;
pub use imported_history::{IMPORTED_HISTORY_PREFIX, Imported, ImportedHistory};
mod keeping_devices;
pub use keeping_devices::{
    WORKSPACE_KEEPING_DEVICES_KEY, holder_role, keeping_device_keys_of, keeping_devices_of,
};
pub(crate) mod change_hashes;
pub(crate) mod seed;
mod workspace_name;
mod workspace_settings;
use held::Held;
pub use held::{MAX_HELD_BYTES, MAX_HELD_FOR_GOVERNANCE_PER_WRITER, MAX_HELD_INPUTS};
pub use seed::{seed_workspace, seed_workspace_at, workspace_seed};
mod checkpoints;
pub(crate) use checkpoints::kept_by as checkpoint_check_kept_by;
pub use checkpoints::{CHECKPOINT_CHECKS_KEY, CheckpointCheck};
mod compaction_set;
mod quarantine;
mod sealed_compaction;
pub use quarantine::Quarantined;
mod sealed_history;
mod unsent_bases;
pub use sealed_compaction::{
    HeldDocument, MergedDocument, OpenSealedError, OpenedDocument, OpenedHistory, OpenedWrite,
    SealCheckpoint, UnmergedReason, UnmergedWrite,
};
/// The peer's own subjects: how a replica is built and who joins it, the
/// authority a device holds there, the records it keeps, what it takes in and
/// admits, what it signs and seals, and the durable record it restores from.
mod admitting;
mod authority;
pub use authority::DocumentKeysBasis;
mod construction;
mod durable;
mod written_under;
pub use durable::DURABLE_VERSION;
#[cfg(test)]
pub(crate) use durable::Durable;
#[cfg(test)]
use durable::brought_up;
pub(crate) use durable::{DocumentParts, DurableParts};
mod copied;
mod receiving;
mod records;
mod sealing;

#[expect(
    clippy::needless_pass_by_value,
    reason = "a `map_err` adapter, which hands over the error by value"
)]
fn err(error: impl ToString) -> String {
    error.to_string()
}
/// The person a member's access is for.
pub(crate) fn person_of(access: &MemberAccess) -> Result<&str, String> {
    access
        .person_id()
        .ok_or_else(|| "the access is for no person".to_owned())
}
pub(crate) fn now_ns(now: SystemTime) -> u64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .try_into()
        .unwrap_or(u64::MAX)
}
/// The time automerge stamps a change with: seconds since the Unix epoch.
pub(crate) fn now_secs(now: SystemTime) -> i64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

/// The id of the grant that makes a member an administrator.
fn administrator_grant_id(user_public_key: [u8; 32]) -> GrantId {
    tilo_action_policy::roles::administrator_grant_id(&hex::encode(user_public_key))
}
/// The decision context of a policy question asked at `now`.
pub fn peer_handle(key: &[u8; 32]) -> u64 {
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, key);
    std::hash::Hasher::finish(&hasher)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SeedDocument {
    pub app_id: String,
    pub document_id: String,
    pub kind: String,
    pub bytes: Vec<u8>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SeedState {
    pub workspace_changes: Vec<Vec<u8>>,
    pub documents: Vec<SeedDocument>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernedAccess {
    /// The devices the workspace admits now: its members' certified,
    /// unrevoked devices.
    pub member_devices: Vec<[u8; 32]>,
    /// Its members, by their roots: every person its policy gives a role,
    /// whether or not a device of theirs is listed now.
    pub member_user_roots: Vec<[u8; 32]>,
}
#[derive(Clone, Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "public outcome record: each flag reports an independent fact"
)]
pub struct ReceiveOutcome {
    pub changed: bool,
    pub deferred: bool,
    pub duplicate: bool,
    pub rejected: usize,
    pub metadata_changed: bool,
    pub incomplete: Option<String>,
    /// Held app messages let go unapplied, each with why
    /// ([`StatePeer`]'s prerequisite queue): never a refusal of the input
    /// being admitted.
    pub dropped: Vec<DroppedInput>,
    /// Something a message let go earlier waited for has landed: ask the
    /// workspace's host to replay what it retains, so that message is decided
    /// again.
    pub replay_wanted: bool,
    /// The input left a workspace change or record write waiting for history
    /// this peer lacks: its sender is asked for its state (`SyncReq`).
    pub lacks_history: bool,
}
/// Authenticated carrier inputs. A native publication requires the embedding
/// adapter's actual pinned owner/session/cursor proof, not an inert event tag.
#[derive(Clone, Copy, Debug)]
pub enum TransportOrigin {
    ServerDelta([u8; 32]),
    RetainedServerDelta([u8; 32]),
    RetainedNativePublication { publisher: [u8; 32] },
    UntrustedPresence([u8; 32]),
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct Route {
    app: String,
    record: String,
}

impl Route {
    /// The route of `document` of `app`.
    fn of(app: &str, document: &str) -> Self {
        Self {
            app: app.into(),
            record: document.into(),
        }
    }

    /// The route as `(app, document)`.
    fn named(&self) -> (String, String) {
        (self.app.clone(), self.record.clone())
    }
}
// Documents a durable hash took again: those changed since it was last taken.
#[cfg(test)]
thread_local! { pub(crate) static DOCUMENTS_WALKED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
thread_local! { pub(crate) static CHECKPOINT_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
thread_local! { pub(crate) static CERTIFICATE_VERIFICATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[derive(Clone)]
struct AcceptedCheckpoint(std::sync::Arc<CheckpointBytes>);
impl PartialEq for AcceptedCheckpoint {
    fn eq(&self, other: &Self) -> bool {
        self.0.bytes == other.0.bytes
    }
}
struct CheckpointBytes {
    bytes: Vec<u8>,
    decoded: std::sync::OnceLock<Result<std::sync::Arc<DecodedCheckpoint>, String>>,
}
struct DecodedCheckpoint {
    doc: AutoCommit,
    heads: Vec<String>,
    payload_hash: [u8; 32],
}
impl AcceptedCheckpoint {
    fn new(bytes: Vec<u8>) -> Self {
        Self(std::sync::Arc::new(CheckpointBytes {
            bytes,
            decoded: std::sync::OnceLock::new(),
        }))
    }
    fn decoded(&self) -> Result<std::sync::Arc<DecodedCheckpoint>, String> {
        // Only immutable bytes and their derived representation are shared.
        // Receipt authority, route and current coverage remain live decisions.
        self.0.decoded.get_or_init(|| self.decode()).clone()
    }
    fn decode(&self) -> Result<std::sync::Arc<DecodedCheckpoint>, String> {
        #[cfg(test)]
        CHECKPOINT_DECODES.with(|count| count.set(count.get().saturating_add(1)));
        let mut doc = AutoCommit::load(&self.0.bytes).map_err(err)?;
        let heads = doc.get_heads().iter().map(ToString::to_string).collect();
        let payload_hash = crate::document::snapshot_hash(&self.0.bytes).map_err(err)?;
        Ok(std::sync::Arc::new(DecodedCheckpoint {
            doc,
            heads,
            payload_hash,
        }))
    }
}

type Document = PeerDocument;
#[derive(Serialize, Deserialize)]
struct DurableDocument {
    route: Route,
    bytes: Vec<u8>,
    watermarks: Vec<([u8; 32], u64)>,
    encryption_required: bool,
    covered: BTreeMap<String, u64>,
    covered_snapshot: Option<Vec<u8>>,
}
/// Each encrypted record's sealed writes, in admission order.
type SealedHistory = Vec<(Route, Vec<SignedOp>)>;

/// Most sealed writes, and their bytes, one record keeps for a key holder
/// since its last compaction. Past them a key holder compacts the history
/// ([`PeerPublication::draft_sealed_compaction`]); the compaction set itself
/// counts toward its own bound ([`MAX_COMPACTION_SET_BYTES`]), so the work a
/// compaction costs tracks the writes since the last one, not the record's
/// size. This crate's tests keep fewer, so a test crosses the bound many
/// times over in the writes it can afford.
#[cfg(not(test))]
pub(crate) const MAX_SEALED_OPS: usize = 4096;
#[cfg(test)]
pub(crate) const MAX_SEALED_OPS: usize = 256;
pub(crate) const MAX_SEALED_BYTES: usize = 16 * 1024 * 1024;
/// Most payload bytes of a record's compaction set, the compaction and the
/// segments it names: a snapshot as large as one document transfer, with
/// workspace for its pieces' envelopes (a few bytes each) and the writers it
/// covers.
const MAX_COMPACTION_SET_BYTES: usize =
    collab_types::document_transfer::MAX_DOCUMENT_TRANSFER_BYTES + 1024 * 1024;
/// Most ops of a compaction set: one transfer in segments no smaller than a
/// receiver of a transfer accepts.
const MAX_COMPACTION_SET_OPS: usize = collab_types::document_transfer::MAX_DOCUMENT_TRANSFER_BYTES
    / collab_types::document_transfer::MIN_DOCUMENT_SEGMENT_BYTES
    + 1;

/// The length and digest of one stretch of a durable encoding: a part's bytes, or a document's
/// changes since its base.
pub(crate) type StretchDigest = (u64, [u8; 32]);

/// Whether a record's sealed history, a compaction set of `set` ops and bytes
/// and `writes` ops and bytes after it, is within its bounds.
const fn sealed_within_bounds(set: (usize, usize), writes: (usize, usize)) -> bool {
    set.0 <= MAX_COMPACTION_SET_OPS
        && set.1 <= MAX_COMPACTION_SET_BYTES
        && writes.0 <= MAX_SEALED_OPS
        && writes.1 <= MAX_SEALED_BYTES
}

#[derive(Clone)]
pub struct StatePeer {
    core: PeerCore,
    /// The app surfaces received changes are decided against by their
    /// claims; without a source, a change is decided as an edit of its
    /// record. Set by the host, never persisted.
    surfaces: collab_types::Surfaces,
    /// The app surface each app's changes this peer signs are decided
    /// against, by app id: what the host launched the app with.
    app_surfaces: AppSurfaces,
    /// What app messages let go unapplied waited for. Never persisted.
    missed: held::Missed,
    /// The pieces of segmented compactions not yet whole. Never persisted.
    gathering: compaction_set::Gathering,
    /// Whether this is a workspace host's copy of the workspace
    /// ([`Self::write_as_workspace_host`]). Set by the host, never persisted.
    workspace_host: bool,
    /// The device whose copy this is, once the device's own program said so
    /// ([`Self::write_as_device`]): what lets the copy keep, as that device's
    /// Never persisted.
    own_device: Option<[u8; 32]>,
    /// The governance this copy holds now, when this peer is the copy as it
    /// judges a write by the governance the write was written under
    /// ([`Self::judged_under`]): what a cut kept since is read from. Boxed:
    /// every copy carries it, and only one judging a write holds one.
    governance_now: Option<Box<CachedDocument>>,
    /// Device keys this copy found signing two different changes under one sequence number,
    /// running on two machines ([`copied`]): their changes are refused until the person re-adds
    /// one of the machines. Never persisted: a copy that starts again finds the clash again.
    copied: std::collections::BTreeMap<[u8; 32], copied::Copied>,
    /// The governance action this copy writes while it makes one
    /// ([`Self::stamp_action`]): the verb and the person or bot every
    /// governance change it commits names. Never persisted: it is set for one
    /// administration and cleared when it is committed.
    action_stamp: Option<governance_writes::ActionStamp>,
}

/// The Automerge actor a workspace host's own changes to a workspace are made under,
/// for the host signing with `device`: never the device key itself. A
/// device that hosts its own workspaces holds two copies of each, its replica
/// and its host's, both signing with the same key; two documents writing
/// under one actor number their changes independently, and each refuses
/// the other's duplicates. Who wrote a change is never read from its
/// actor: admission authenticates the writer by the change's transport
/// source and its certificate.
#[must_use]
pub fn workspace_host_actor(device: &[u8; 32]) -> automerge::ActorId {
    // The salt is the namespace a host's changes were already made under: a
    // workspace's stored history keeps its host's actor, whatever the code
    // around it is called now.
    automerge::ActorId::from(blake3::derive_key("tilo workspace host actor v1", device).to_vec())
}

fn defaults() -> &'static std::sync::Mutex<(collab_types::Surfaces, AppSurfaces)> {
    static DEFAULTS: std::sync::OnceLock<std::sync::Mutex<(collab_types::Surfaces, AppSurfaces)>> =
        std::sync::OnceLock::new();
    DEFAULTS.get_or_init(std::sync::Mutex::default)
}

fn process_default() -> (collab_types::Surfaces, AppSurfaces) {
    // A panic that killed a thread mid-update leaves the defaults as they
    // were: they are whole values, set once and read whole.
    defaults().lock_recover().clone()
}

/// The record `message` writes and its sequence, if it is a document write.
fn written_route(message: &NeighbourMsg) -> Option<(Route, u64)> {
    match message {
        NeighbourMsg::AppDocument {
            app_id,
            document_id,
            envelope,
            ..
        } => Some((
            Route {
                app: app_id.clone(),
                record: document_id.clone(),
            },
            envelope.seq,
        )),
        _ => None,
    }
}

/// The app surfaces this process decides received changes against
/// ([`decide_with_surfaces`]).
#[must_use]
pub fn default_surfaces() -> collab_types::Surfaces {
    process_default().0
}

/// Decide the changes every peer of this process receives against the app
/// surfaces `surfaces` finds, and name in the changes it signs the surface
/// `of` returns for each app: what a host sets once, before it opens or
/// restores its workspaces. A peer made before keeps what it had.
pub fn decide_with_surfaces(
    surfaces: collab_types::Surfaces,
    of: impl Fn(&str) -> Option<[u8; 32]> + Send + Sync + 'static,
) {
    collab_types::Surfaces::set_process(surfaces.clone());
    *defaults().lock_recover() = (surfaces, AppSurfaces(Some(std::sync::Arc::new(of))));
}

/// The surface hash of an app's package, by app id.
#[derive(Clone, Default)]
struct AppSurfaces(Option<std::sync::Arc<SurfaceOf>>);

type SurfaceOf = dyn Fn(&str) -> Option<[u8; 32]> + Send + Sync;

/// The record as this peer holds it, when it has units beside itself to
/// tell `envelope`'s changes' writes apart by, or the changes bring it its
/// first unit index (its app gained a unit after the record was made): the
/// writes of those after it are told apart by that index. A record without
/// units is not copied for every change it receives: an empty record stands
/// in, by which each change writes the record itself, its only unit.
fn held_units(
    doc: &crate::cached_document::CachedDocument,
    envelope: &SignedOp,
    opened: Option<&[automerge::Change]>,
) -> AutoCommit {
    let brings_index = || {
        if let Some(opened) = opened {
            return collab_types::units::brings_index(opened);
        }
        envelope.encryption.is_none()
            && tilo_failure::ignored!(
                "change bytes that do not parse bring no index",
                collab_types::DocumentMutation::decode(&envelope.payload)
                    .ok()
                    .and_then(|mutation| collab_admission::parse_changes(mutation.data()).ok())
            )
            .is_some_and(|changes| collab_types::units::brings_index(&changes))
    };
    if collab_types::units::has_units(&**doc) || brings_index() {
        AutoCommit::clone(doc)
    } else {
        AutoCommit::new()
    }
}

/// The surface a record document's identity names (`tilo:record.surface`).
fn document_surface(doc: &impl automerge::ReadDoc) -> Option<[u8; 32]> {
    let (_, meta) = doc.get(automerge::ROOT, "tilo:record").ok()??;
    match doc.get(&meta, "surface").ok()?? {
        (automerge::Value::Scalar(hash), _) => hex::decode(hash.as_str()?).ok()?.try_into().ok(),
        _ => None,
    }
}

/// The kind a record document's identity names (`tilo:record.kind`).
fn resource_type(doc: &impl automerge::ReadDoc) -> Option<String> {
    let (_, meta) = doc.get(automerge::ROOT, "tilo:record").ok()??;
    match doc.get(&meta, "kind").ok()?? {
        (automerge::Value::Scalar(kind), _) => kind.as_str().map(str::to_owned),
        _ => None,
    }
}

fn check_governed_snapshot(
    workspace: Option<&AutoCommit>,
    route: &Route,
    covered: &BTreeMap<String, u64>,
    snapshot: Option<&[u8]>,
) -> Result<(), String> {
    use crate::coverage::read::{CoverageMatchError, governed_snapshot_matches};
    governed_snapshot_matches(workspace, (&route.app, &route.record), covered, snapshot)
    .map(drop)
    .map_err(|error| match error {
        CoverageMatchError::Decode(error) => format!("invalid checkpoint coverage: {error}"),
        CoverageMatchError::NoMatch => format!(
            "governed checkpoint for {}/{} does not match its coverage; preserve journal and request a fresh authenticated checkpoint",
            route.app, route.record
        ),
    })
}

/// Whether `held` and `incoming` are one device's certificate, the later one
/// replacing the earlier: the same device key under the same person, granting
/// no more, and issued no earlier. A workspace keeps the certificate it holds
/// for a device, and a device certified again — its person's root re-signing
/// it, at the same instant or later — is the same member, not a conflict.
pub(crate) fn supersedes(held: &DeviceCert, incoming: &DeviceCert) -> bool {
    held.device_public_key == incoming.device_public_key
        && held.user_public_key == incoming.user_public_key
        && incoming
            .capabilities
            .grants_no_more_than(&held.capabilities)
        && incoming.issued_at_ns >= held.issued_at_ns
}

pub use governed_rows::RowError;
pub(crate) use governed_rows::{GovernedIdentity, RedemptionReceipt};
pub(crate) use invitation_receipts::StoredReceipt;

/// The map at `key` of a governed workspace document.
pub(crate) fn governed_map(doc: &AutoCommit, key: &str) -> Result<automerge::ObjId, String> {
    doc.get(ROOT, key)
        .map_err(err)?
        .map(|(_, id)| id)
        .ok_or_else(|| format!("governed {key} missing"))
}

#[cfg(test)]
#[path = "state/delivery_admission_resource_tests.rs"]
mod delivery_admission_resource_tests;

#[cfg(test)]
#[path = "state/administrators_tests.rs"]
mod administrators_tests;
#[cfg(test)]
#[path = "state/admitting_tests.rs"]
mod admitting_tests;
#[cfg(test)]
#[cfg(test)]
#[path = "state/durable_tests.rs"]
mod durable_tests;
#[cfg(test)]
#[path = "state/governance_attribution_tests.rs"]
mod governance_attribution_tests;
#[cfg(test)]
#[path = "state/governance_cache_tests.rs"]
mod governance_cache_tests;
#[cfg(test)]
#[path = "state/ownership_tests.rs"]
mod ownership_tests;

#[cfg(test)]
#[path = "state/founded_rows_tests.rs"]
mod founded_rows_tests;
mod governed_rows;
#[cfg(test)]
#[path = "state/held_document_tests.rs"]
mod held_document_tests;
#[cfg(test)]
#[path = "state/held_queue_tests.rs"]
mod held_queue_tests;
pub(crate) mod invitation_receipts;
#[cfg(test)]
#[path = "state/joined_tests.rs"]
mod joined_tests;
#[cfg(test)]
#[path = "state/member_list_tests.rs"]
mod member_list_tests;
mod rebase;
#[cfg(test)]
#[path = "state/removal_tests.rs"]
mod removal_tests;
#[cfg(test)]
#[path = "state/roster_beside_governance_tests.rs"]
mod roster_beside_governance_tests;
#[cfg(test)]
#[path = "state/sealed_compaction_tests.rs"]
mod sealed_compaction_tests;
#[cfg(test)]
#[cfg(test)]
#[path = "state/sharing_tests.rs"]
mod sharing_tests;
#[cfg(test)]
#[path = "state/superseded_checkpoint_tests.rs"]
mod superseded_checkpoint_tests;
