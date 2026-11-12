//! Certified workspace and document replica. Transport identity is always an input.
use crate::admission::{
    AdmissionError, AdmissionReport, DocumentAdmission, DroppedInput, ExactAdmissions,
    InputDisposition, MissingPrerequisite,
};
use crate::cached_document::CachedDocument;
use crate::verified_certificate::VerifiedCertificate;
use crate::{NeighbourMsg, workspace};
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, transaction::Transactable};
use collab_types::caps::Principal;
use collab_types::{
    ArtifactKind, ChannelState, DocumentMutation, Ed25519PublicKey, OpKind, Role, SignedOp,
};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use tilo_action_policy::{
    ActionKind, Answer, Authority, Grant, GrantId, Subject, TreePath, WorkspacePolicy,
};
use tilo_lock::MutexRecover;
use tilo_user_identity::{DeviceCert, RevocationList};
pub use tilo_workspace_join::{DocumentRow, GovernedMember};
use web_time::{Duration, SystemTime};
mod backend;
mod document_capture;
pub use document_capture::{
    KeptRecord, PeerDocumentCapture, PeerDocumentReadError, changes_since,
    committed_document_revision, saved_whole,
};
#[cfg(test)]
pub(crate) mod json_resource_tests;
use crate::shared::Shared;
use backend::{PeerCore, PeerDocument};
mod bans;
mod covered_records;
mod durable_before_owner;
pub(crate) use durable_before_owner::{
    DURABLE_BASE_VERSION, DurableV4, DurableV5, DurableV6, GOVERNANCE_LAYOUT_BEFORE,
    durable_base_version,
};
mod member_records;
use member_records::{empty_genesis, empty_genesis_op};
mod facades;
mod governance;
mod governance_writes;
pub use facades::{
    PeerGovernance, PeerGovernanceMut, PeerMediaGovernance, PeerMediaGovernanceMut, PeerOutbox,
    PeerOutboxMut, PeerPublication, PeerPublicationMut, PeerWorkspace, PeerWorkspaceMut,
};
mod app_versions;
pub use app_versions::InstalledApp;
mod bot_members;
pub(crate) mod certificate_renewal;
mod delegates;
pub(crate) use bot_members::requests_in as bot_requests_in;
pub use bot_members::{InstalledBot, RequestedBot};
pub use delegates::{DelegateInactive, GovernedAgent, GovernedDelegateRow};
mod agent_runs;
mod local_admission;
mod local_outbox;
mod membership_error;
pub use membership_error::MembershipError;
mod owner_membership;
mod workspace_delta;
mod workspace_effects;
mod workspace_membership;
mod workspace_refusals;
mod workspace_shortcuts;
pub use local_outbox::Reseal;
mod governed_coverage;
mod held;
mod joined;
mod media_governance;
mod native_publication;
mod publication;
mod workspace_hosts;
pub use workspace_hosts::{WORKSPACE_HOSTS_KEY, workspace_hosts_of};
mod personal;
mod seed;
mod workspace_name;
mod workspace_settings;
use held::Held;
pub use held::{MAX_HELD_BYTES, MAX_HELD_INPUTS};
pub use native_publication::NativePublicationCut;
pub use personal::link::{AdmittedFrame, PERSONAL_ALPN, PersonalSync};
pub use personal::{PeerPersonal, PersonalStore};
pub use seed::{seed_workspace, workspace_seed};
mod compaction_set;
mod sealed_compaction;
mod sealed_history;
pub use sealed_compaction::{
    OpenedHistory, OpenedRecord, OpenedWrite, SealCheckpoint, UnmergedReason, UnmergedWrite,
};
/// The peer's own subjects: how a replica is built and who joins it, the
/// authority a device holds there, the records it keeps, what it takes in and
/// admits, what it signs and seals, and the durable record it restores from.
mod admitting;
mod authority;
mod construction;
mod durable;
mod receiving;
mod records;
mod sealing;
pub use authority::MemberAuthority;

#[expect(
    clippy::needless_pass_by_value,
    reason = "a `map_err` adapter, which hands over the error by value"
)]
fn err(error: impl ToString) -> String {
    error.to_string()
}
pub(crate) fn now_ns(now: SystemTime) -> u64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// Whether `agent` is admitted to the workspace: it holds a live grant on the workspace
/// itself.
pub(crate) fn agent_is_member(policy: &WorkspacePolicy, agent: &str, now: SystemTime) -> bool {
    let cx = tilo_action_policy::Context::at_time(now);
    let workspace = policy.node();
    policy.grants.iter().any(|grant| {
        matches!(&grant.subject, Subject::Agent(held) if held == agent)
            && grant.node == workspace
            && grant.answer != Answer::Deny
            && grant.is_live(&cx)
    })
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
    pub record_id: String,
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
    pub member_devices: Vec<[u8; 32]>,
    pub member_user_roots: Vec<[u8; 32]>,
}
/// Who a Read Deny keeps out of a workspace's record channels; see
/// [`PeerGovernance::withheld_record_readers`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordReaders {
    /// Every device a record's channel admits where nothing withholds it from
    /// the record: the workspace's member devices.
    pub readers: BTreeSet<[u8; 32]>,
    /// Each channel whose record the caller can name, and the devices that
    /// may not read that record (possibly none).
    pub withheld: std::collections::HashMap<collab_types::ChannelId, BTreeSet<[u8; 32]>>,
    /// The devices kept out of every channel whose record the caller cannot
    /// name: every device [`Self::readers`] lists. Such a channel may be any
    /// record, a member's own (`member:<root>:<kind>`, read by that person
    /// alone) included, so no one is admitted to it until a device names its
    /// record.
    pub unrouted: BTreeSet<[u8; 32]>,
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
    RelayDelta([u8; 32]),
    RetainedRelayDelta([u8; 32]),
    RetainedNativePublication { publisher: [u8; 32] },
    UntrustedPresence([u8; 32]),
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct Route {
    app: String,
    record: String,
}
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

/// Most sealed writes, and their bytes, one record keeps for a key holder.
/// Past them a key holder compacts the history
/// ([`PeerPublication::draft_sealed_compaction`]). This crate's tests keep
/// fewer, so a test crosses the bound many times over in the writes it can
/// afford.
#[cfg(not(test))]
const MAX_SEALED_OPS: usize = 4096;
#[cfg(test)]
const MAX_SEALED_OPS: usize = 256;
const MAX_SEALED_BYTES: usize = 16 * 1024 * 1024;

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
    /// Whether [`Self::restore`] brought the governance up from an earlier
    /// stored layout ([`workspace::layout`]): a device that governs the
    /// workspace then publishes it anew ([`Self::governance_brought_up`]).
    brought_up: bool,
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
    automerge::ActorId::from(blake3::derive_key("tilo room host actor v1", device).to_vec())
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
            record_id,
            envelope,
            ..
        } => Some((
            Route {
                app: app_id.clone(),
                record: record_id.clone(),
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
fn held_units(doc: &crate::cached_document::CachedDocument, envelope: &SignedOp) -> AutoCommit {
    let brings_index = || {
        envelope.encryption.is_none()
            && collab_types::DocumentMutation::decode(&envelope.payload)
                .ok() // ignored-failure: change bytes that do not parse bring no index
                .and_then(|mutation| collab_types::parse_changes(mutation.data()).ok())
                .is_some_and(|changes| collab_types::units::brings_index(&changes))
    };
    if collab_types::units::has_units(&**doc) || brings_index() {
        AutoCommit::clone(doc)
    } else {
        AutoCommit::new()
    }
}

/// The surface a record document's identity names (`tilo:record.surface`).
fn record_surface(doc: &impl automerge::ReadDoc) -> Option<[u8; 32]> {
    let (_, meta) = doc.get(automerge::ROOT, "tilo:record").ok()??;
    match doc.get(&meta, "surface").ok()?? {
        (automerge::Value::Scalar(hash), _) => hex::decode(hash.as_str()?).ok()?.try_into().ok(),
        _ => None,
    }
}

/// The kind a record document's identity names (`tilo:record.kind`).
fn record_kind(doc: &impl automerge::ReadDoc) -> Option<String> {
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
    use crate::coverage::read::{CoverageMatchError, LegacyCoverage, governed_snapshot_matches};
    let workspace = workspace.map(crate::workspace::read::LegacyRead);
    governed_snapshot_matches(
        workspace.as_ref(),
        (&route.app, &route.record),
        covered,
        snapshot,
        &LegacyCoverage,
    )
    .map(drop)
    .map_err(|error| match error {
        CoverageMatchError::Decode(error) => format!("invalid checkpoint coverage: {error}"),
        CoverageMatchError::Capacity => "checkpoint coverage exceeds its limit".into(),
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
pub(crate) use governed_rows::WorkspaceDelegation;
#[cfg(test)]
pub(crate) use governed_rows::WorkspaceDelegationPermissions;
pub(crate) use governed_rows::{GovernedIdentity, RedemptionReceipt};
pub(crate) use invitation_ledger::LedgerReceipt;

fn member_role_from_role(role: Role) -> Option<crate::join::MemberRole> {
    match role {
        Role::Viewer => Some(crate::join::MemberRole::Viewer),
        Role::Commenter => Some(crate::join::MemberRole::Commenter),
        Role::Editor => Some(crate::join::MemberRole::Editor),
        Role::Moderator => Some(crate::join::MemberRole::Moderator),
        Role::Owner => None,
    }
}

/// The map at `key` of a governed workspace document.
fn governed_map(doc: &AutoCommit, key: &str) -> Result<automerge::ObjId, String> {
    doc.get(ROOT, key)
        .map_err(err)?
        .map(|(_, id)| id)
        .ok_or_else(|| format!("governed {key} missing"))
}

/// Record `user_public_key`'s role in the workspace's `members` map.
pub(crate) fn put_member(
    doc: &mut AutoCommit,
    members: &automerge::ObjId,
    user_public_key: [u8; 32],
    role: Role,
) -> Result<(), String> {
    doc.put(
        members,
        hex::encode(user_public_key),
        automerge::ScalarValue::Bytes(serde_json::to_vec(&role).map_err(err)?),
    )
    .map_err(err)
}

/// The workspace key recording when each member was admitted: person hex →
/// JSON Unix milliseconds.
pub(crate) const MEMBERS_JOINED: &str = "members_joined_v1";

/// Record that the workspace admitted `user_public_key` at `at_ms` (Unix
/// milliseconds), unless it already holds a time for them: a member's
/// admission time is written once and kept through role changes and later
/// device admissions.
pub(crate) fn put_member_joined_once(
    doc: &mut AutoCommit,
    user_public_key: [u8; 32],
    at_ms: u64,
) -> Result<(), String> {
    let joined = match doc.get(ROOT, MEMBERS_JOINED).map_err(err)? {
        Some((automerge::Value::Object(ObjType::Map), id)) => id,
        Some(_) => return Err(format!("governed {MEMBERS_JOINED} is not a map")),
        None => doc
            .put_object(ROOT, MEMBERS_JOINED, ObjType::Map)
            .map_err(err)?,
    };
    let person = hex::encode(user_public_key);
    if doc.get(&joined, person.as_str()).map_err(err)?.is_some() {
        return Ok(());
    }
    doc.put(
        &joined,
        person,
        automerge::ScalarValue::Bytes(serde_json::to_vec(&at_ms).map_err(err)?),
    )
    .map_err(err)
}

/// Forget when `person` was admitted, as they leave the workspace.
pub(crate) fn delete_member_joined(doc: &mut AutoCommit, person: &str) -> Result<(), String> {
    let Some((_, joined)) = doc.get(ROOT, MEMBERS_JOINED).map_err(err)? else {
        return Ok(());
    };
    if doc.get(&joined, person).map_err(err)?.is_some() {
        doc.delete(&joined, person).map_err(err)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "state/delivery_admission_resource_tests.rs"]
mod delivery_admission_resource_tests;

#[cfg(test)]
#[path = "state/media_tests.rs"]
mod workspace_media_native_tests;

#[cfg(test)]
#[path = "state/bot_members_tests.rs"]
pub(crate) mod bot_members_tests;

#[cfg(test)]
#[path = "state/administrators_tests.rs"]
mod administrators_tests;
#[cfg(test)]
#[path = "state/admitting_tests.rs"]
mod admitting_tests;
#[cfg(test)]
#[path = "state/governance_cache_tests.rs"]
mod governance_cache_tests;
mod governed_rows;
#[cfg(test)]
#[path = "state/held_queue_tests.rs"]
mod held_queue_tests;
pub(crate) mod invitation_ledger;
#[cfg(test)]
#[path = "state/joined_tests.rs"]
mod joined_tests;
#[cfg(test)]
#[path = "state/layout_restore_tests.rs"]
mod layout_restore_tests;
mod rebase;
#[cfg(test)]
#[path = "state/roster_beside_governance_tests.rs"]
mod roster_beside_governance_tests;
#[cfg(test)]
#[path = "state/sealed_compaction_tests.rs"]
mod sealed_compaction_tests;
#[cfg(test)]
#[path = "state/sealed_tests.rs"]
mod sealed_tests;
#[cfg(test)]
#[path = "state/sharing_tests.rs"]
mod sharing_tests;
#[cfg(test)]
#[path = "state/superseded_checkpoint_tests.rs"]
mod superseded_checkpoint_tests;
