//! Certified room and document replica. Transport identity is always an input.
use crate::admission::{
    AdmissionError, AdmissionReport, DocumentAdmission, ExactAdmissions, InputDisposition,
    MissingPrerequisite,
};
use crate::cached_document::CachedDocument;
use crate::verified_certificate::VerifiedCertificate;
use crate::{NeighbourMsg, document, room};
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, transaction::Transactable};
use collab_types::caps::Principal;
use collab_types::{
    ArtifactKind, ChannelState, DocumentMutation, Ed25519PublicKey, OpKind, Role, SignedOp,
};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use tilo_action_policy::{
    ActionKind, Answer, Authority, Grant, GrantId, RoomPolicy, Subject, TreePath,
};
use tilo_user_identity::{DeviceCert, RevocationList};
pub use tilo_workspace_join::{DocumentRow, GovernedMember};
use web_time::{Duration, SystemTime};
mod backend;
mod bounded;
pub use bounded::{
    BoundedPeerError, BoundedPeerInvalid, BoundedStatePeer, PeerDeliveryPolicy,
    PeerDeliveryPolicyError, PeerDocumentCapture, PeerDocumentRead, PeerDocumentReadError,
    PeerLimits, PeerMediaCall, PeerMember, PeerPolicyBasis, PeerRead, PeerRevocations,
    PeerRoomAccess, committed_document_revision,
};
#[cfg(test)]
pub(crate) mod json_resource_tests;
use crate::shared::Shared;
use backend::{LegacyNative, PeerCore, PeerDocument};
mod bans;
mod facades;
mod governance;
pub use facades::{
    PeerGovernance, PeerGovernanceMut, PeerMediaGovernance, PeerMediaGovernanceMut, PeerOutbox,
    PeerOutboxMut, PeerWorkspace, PeerWorkspaceMut,
};
mod bot_members;
mod delegates;
pub use bot_members::InstalledBot;
pub use delegates::{GovernedAgent, GovernedDelegateRow};
mod local_admission;
mod local_outbox;
mod owner_membership;
mod topology_namespace;
mod workspace_effects;
mod workspace_membership;
mod workspace_tasks;
pub use local_outbox::{LocalRecoveryReservation, Reseal};
mod media_governance;
mod native_publication;
mod room_name;
pub use native_publication::NativePublicationCut;
mod resources;
mod sealed_compaction;
mod sealed_history;
pub use sealed_compaction::{SealCheckpoint, SealDelta};

#[expect(
    clippy::needless_pass_by_value,
    reason = "a `map_err` adapter, which hands over the error by value"
)]
fn err(error: impl ToString) -> String {
    error.to_string()
}
fn now_ns(now: SystemTime) -> u64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .try_into()
        .unwrap_or(u64::MAX)
}
fn now_secs(now: SystemTime) -> u64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
/// A current member's authority in the room: who they are, their role, the
/// room's policy as admitted, and whether their device is held to the
/// foreign-device bound.
#[derive(Clone, Debug)]
pub struct MemberAuthority {
    pub role: Role,
    pub person: String,
    pub policy: RoomPolicy,
    pub foreign: bool,
    /// The bound on a delegate's device, an agent's or a bot's: its
    /// person's grants within its own, never above an Editor's.
    pub delegate: Option<tilo_action_policy::Bound>,
}
impl MemberAuthority {
    /// The member's standing in the room, held to the foreign-device bound
    /// on a foreign device: what a receiver decides their ops with.
    #[must_use]
    pub fn standing(self) -> tilo_action_policy::Standing {
        let mut standing = tilo_action_policy::Standing::person(self.policy, self.person);
        if self.foreign {
            standing = standing.on_foreign_device();
        }
        if let Some(bound) = self.delegate {
            standing = standing.with_bound(bound);
        }
        standing
    }

    /// The member's authority, held to `foreign_bound` on a foreign device.
    #[must_use]
    pub fn authority<'a>(&'a self, foreign_bound: &'a [Grant]) -> Authority<'a> {
        let authority = self
            .policy
            .person_on_device(&self.person, self.foreign, foreign_bound);
        match &self.delegate {
            Some(bound) => authority.bounded_by(bound.party(&self.policy.grants)),
            None => authority,
        }
    }
    /// Whether the member may perform an action of `kind` at `node` at `now`.
    #[must_use]
    pub fn allows(&self, node: &TreePath, kind: ActionKind, now: SystemTime) -> bool {
        let bound = tilo_action_policy::roles::foreign_device_bound();
        self.authority(&bound)
            .decide(node, kind, &tilo_action_policy::Context::at_time(now))
            .answer
            == Answer::Allow
    }
}
/// Whether `agent` is admitted to the room: it holds a live grant on the room
/// itself.
pub(crate) fn agent_is_member(policy: &RoomPolicy, agent: &str, now: SystemTime) -> bool {
    let cx = tilo_action_policy::Context::at_time(now);
    let room = policy.node();
    policy.grants.iter().any(|grant| {
        matches!(&grant.subject, Subject::Agent(held) if held == agent)
            && grant.node == room
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
    pub room_changes: Vec<Vec<u8>>,
    pub documents: Vec<SeedDocument>,
}
/// What [`StatePeer::plaintext_changes_since`] found: the change bytes and
/// the heads of the document they complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaintextChanges {
    pub heads: Vec<automerge::ChangeHash>,
    pub bytes: Vec<u8>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernedAccess {
    pub member_devices: Vec<[u8; 32]>,
    pub member_user_roots: Vec<[u8; 32]>,
}
/// Who a Read Deny keeps out of a room's record channels; see
/// [`PeerGovernance::withheld_record_readers`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordReaders {
    /// Each channel whose record the steward can name, and the member
    /// devices that may not read that record (possibly none).
    pub withheld: std::collections::HashMap<collab_types::ChannelId, BTreeSet<[u8; 32]>>,
    /// The member devices kept out of every channel whose record the steward
    /// cannot name: those a Deny over a pattern of records could keep out.
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

type Document = PeerDocument<LegacyNative>;
#[derive(Serialize, Deserialize)]
struct DurableDocument {
    route: Route,
    bytes: Vec<u8>,
    watermarks: Vec<([u8; 32], u64)>,
    encryption_required: bool,
    covered: BTreeMap<String, u64>,
    covered_snapshot: Option<Vec<u8>>,
}
/// [`DurableDocument`] as it encodes, borrowed from the peer.
#[derive(Serialize)]
struct DurableDocumentRef<'a> {
    route: &'a Route,
    bytes: &'a [u8],
    watermarks: Vec<([u8; 32], u64)>,
    encryption_required: bool,
    covered: &'a BTreeMap<String, u64>,
    covered_snapshot: Option<&'a [u8]>,
}
#[derive(Serialize, Deserialize)]
struct Durable {
    version: u32,
    room: String,
    authority: DeviceCert,
    room_bytes: Option<Vec<u8>>,
    documents: Vec<DurableDocument>,
    receipts: BTreeMap<String, Vec<u8>>,
    certificates: Vec<DeviceCert>,
    revocations: Vec<tilo_user_identity::RevocationEntry>,
}
#[derive(Serialize, Deserialize)]
struct DurableV4 {
    version: u32,
    base: Durable,
    pending: VecDeque<([u8; 32], NeighbourMsg, bool)>,
    pending_bytes: usize,
}

// The v4 shape stays frozen. Exact evidence is an explicitly selected addition.
#[derive(Serialize, Deserialize)]
struct DurableV5<'a> {
    version: u32,
    base: DurableV4,
    #[serde(borrow)]
    exact: &'a [u8],
}

/// V5 and the sealed history of each encrypted record. Written only while a
/// record holds sealed writes, so every other state keeps its v5 bytes.
#[derive(Serialize, Deserialize)]
struct DurableV6<'a> {
    version: u32,
    base: DurableV4,
    #[serde(borrow)]
    exact: &'a [u8],
    sealed: SealedHistory,
}
/// Each encrypted record's sealed writes, in admission order.
type SealedHistory = Vec<(Route, Vec<SignedOp>)>;

/// How a local write reaches admission: plain, or sealed and opened by its
/// key holder once the sealed envelope passed every check.
pub(crate) enum LocalWrite<'a> {
    Plain,
    Opened(&'a mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>),
}

/// Most sealed writes, and their bytes, one record keeps for a key holder.
/// Past them a key holder compacts the history
/// ([`StatePeer::compact_sealed_history`]).
const MAX_SEALED_OPS: usize = 4096;
const MAX_SEALED_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct StatePeer {
    core: PeerCore<LegacyNative>,
    /// The app surfaces received changes are decided against by their
    /// claims; without a source, a change is decided as an edit of its
    /// record. Set by the host, never persisted.
    surfaces: collab_types::Surfaces,
    /// The app surface each app's changes this peer signs are decided
    /// against, by app id: what the host launched the app with.
    app_surfaces: AppSurfaces,
}

fn defaults() -> &'static std::sync::Mutex<(collab_types::Surfaces, AppSurfaces)> {
    static DEFAULTS: std::sync::OnceLock<std::sync::Mutex<(collab_types::Surfaces, AppSurfaces)>> =
        std::sync::OnceLock::new();
    DEFAULTS.get_or_init(std::sync::Mutex::default)
}

fn process_default() -> (collab_types::Surfaces, AppSurfaces) {
    defaults()
        .lock()
        .map(|defaults| defaults.clone())
        .unwrap_or_default()
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
/// restores its rooms. A peer made before keeps what it had.
pub fn decide_with_surfaces(
    surfaces: collab_types::Surfaces,
    of: impl Fn(&str) -> Option<[u8; 32]> + Send + Sync + 'static,
) {
    collab_types::Surfaces::set_process(surfaces.clone());
    if let Ok(mut defaults) = defaults().lock() {
        *defaults = (surfaces, AppSurfaces(Some(std::sync::Arc::new(of))));
    }
}

/// The surface hash of an app's package, by app id.
#[derive(Clone, Default)]
struct AppSurfaces(Option<std::sync::Arc<SurfaceOf>>);

type SurfaceOf = dyn Fn(&str) -> Option<[u8; 32]> + Send + Sync;

/// The kind a record document's identity names (`tilo:record.kind`).
/// The record as this peer holds it, when it has units beside itself to
/// tell a change's writes apart by.
fn held_units(doc: &crate::cached_document::CachedDocument) -> Option<AutoCommit> {
    collab_types::units::has_units(&**doc).then(|| AutoCommit::clone(doc))
}

/// The surface a record document's identity names (`tilo:record.surface`).
fn record_surface(doc: &impl automerge::ReadDoc) -> Option<[u8; 32]> {
    let (_, meta) = doc.get(automerge::ROOT, "tilo:record").ok()??;
    match doc.get(&meta, "surface").ok()?? {
        (automerge::Value::Scalar(hash), _) => hex::decode(hash.as_str()?).ok()?.try_into().ok(),
        _ => None,
    }
}

fn record_kind(doc: &impl automerge::ReadDoc) -> Option<String> {
    let (_, meta) = doc.get(automerge::ROOT, "tilo:record").ok()??;
    match doc.get(&meta, "kind").ok()?? {
        (automerge::Value::Scalar(kind), _) => kind.as_str().map(str::to_owned),
        _ => None,
    }
}

fn check_governed_snapshot(
    room: Option<&AutoCommit>,
    route: &Route,
    snapshot: Option<&[u8]>,
) -> Result<(), String> {
    use crate::coverage::read::{CoverageMatchError, LegacyCoverage, governed_snapshot_matches};
    let room = room.map(crate::room::read::LegacyRead);
    governed_snapshot_matches(room.as_ref(), &route.app, &route.record, snapshot, &LegacyCoverage)
        .map_err(|error| match error {
            CoverageMatchError::Decode(error) => format!("invalid checkpoint coverage: {error}"),
            CoverageMatchError::Capacity => "checkpoint coverage exceeds its limit".into(),
            CoverageMatchError::NoMatch => format!(
                "governed checkpoint for {}/{} does not match its coverage; preserve journal and request a fresh authenticated checkpoint",
                route.app, route.record
            ),
        })
}

impl StatePeer {
    /// Build a replica for `room` pinned to `authority` and its certified roster.
    ///
    /// # Errors
    ///
    /// Returns an error if `room` is not a UUID, a certificate fails verification at `now`,
    /// or a certificate is revoked.
    pub fn new(
        room: &str,
        authority: DeviceCert,
        roster: Vec<DeviceCert>,
        revocations: RevocationList,
        now: SystemTime,
    ) -> Result<Self, String> {
        let now = now_ns(now);
        let roster = roster
            .into_iter()
            .map(|certificate| VerifiedCertificate::new(certificate, now))
            .collect::<Result<Vec<_>, _>>()?;
        Self::with_roster(room, authority, roster, revocations, now)
    }

    /// A replica of a room another device governs, from what its join
    /// answered: the governing `authority`, which must be valid at `now`, and
    /// the room's `roster` as its governance admitted it. A roster
    /// certificate is the room's evidence, checked by its signature alone:
    /// a guest certified for an hour stays in the roster after its hour, as
    /// on the host, and its expiry is checked whenever it is used. Building
    /// the replica at `now` refused the whole room over one expired guest,
    /// so no device could join a room anyone had watched.
    ///
    /// # Errors
    ///
    /// Returns an error if `room` is not a UUID, `authority` fails
    /// verification at `now`, a roster certificate's signature fails, or a
    /// certificate is revoked.
    pub fn joined(
        room: &str,
        authority: DeviceCert,
        roster: Vec<DeviceCert>,
        revocations: RevocationList,
        now: SystemTime,
    ) -> Result<Self, String> {
        let roster = roster
            .into_iter()
            .map(|certificate| VerifiedCertificate::historical_typed(certificate).map_err(err))
            .collect::<Result<Vec<_>, _>>()?;
        Self::with_roster(room, authority, roster, revocations, now_ns(now))
    }

    fn with_roster(
        room: &str,
        authority: DeviceCert,
        roster: Vec<VerifiedCertificate>,
        revocations: RevocationList,
        now: u64,
    ) -> Result<Self, String> {
        let room = uuid::Uuid::parse_str(room).map_err(err)?.to_string();
        let mut certificates = BTreeMap::new();
        for certificate in roster
            .into_iter()
            .map(Ok)
            .chain([VerifiedCertificate::new(authority.clone(), now)])
        {
            let certificate = certificate?;
            if revocations.is_revoked(&certificate.device_public_key) {
                return Err("device revoked".into());
            }
            certificates.insert(certificate.device_public_key, certificate);
        }
        Ok(Self {
            core: PeerCore {
                room,
                authority,
                certificates: certificates.into(),
                revocations,
                receipts: Shared::default(),
                room_doc: None,
                documents: Shared::default(),
                pending: VecDeque::new(),
                pending_bytes: 0,
                checkpoint_times: Shared::default(),
                exact_admissions: ExactAdmissions::default(),
                admission_v5: false,
                native_cut_publisher: None,
                _retention: (),
            },
            surfaces: process_default().0,
            app_surfaces: process_default().1,
        })
    }
    /// Add a root-certified viewer to existing governed history. Only the pinned
    /// room authority may issue this local governance mutation.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governance writer, the certificate is invalid,
    /// revoked or conflicts with a recorded one, or governance cannot be updated.
    pub fn admit_certificate(
        &mut self,
        cert: DeviceCert,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        if key.verifying_key().to_bytes() != self.core.authority.device_public_key {
            return Err("governance writer is not pinned authority".into());
        }
        self.certificate(&self.core.authority.device_public_key, now)?;
        if cert.user_public_key != self.core.authority.user_public_key {
            return Err("enrollment root mismatch".into());
        }
        tilo_user_identity::verify_device_cert_at(&cert, now_ns(now)).map_err(err)?;
        if self.core.revocations.is_revoked(&cert.device_public_key) {
            return Err("device revoked".into());
        }
        if let Some(old) = self.core.certificates.get(&cert.device_public_key)
            && old.as_ref() != &cert
        {
            return Err("certificate conflict".into());
        }
        let doc = self
            .core
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let id = hex::encode(cert.device_public_key);
        if room::governance_of(Some(&doc))
            .identities
            .get(&id)
            .and_then(|bytes| serde_json::from_slice::<GovernedIdentity>(bytes).ok())
            .is_some_and(|old| old.device_cert == cert)
        {
            return Ok(false);
        }
        let identity = GovernedIdentity {
            device_cert: cert,
            revocations: vec![],
            admission: None,
            admissions: vec![],
            workspace_delegations: vec![],
            delegate: None,
        };
        self.put_identity(doc, &identity, key, now)
    }
    /// Write `identity`'s governed row and roster name into `doc`, this
    /// room's governance, as the pinned authority `key`, and take the change
    /// in as every member will.
    fn put_identity(
        &mut self,
        mut doc: CachedDocument,
        identity: &GovernedIdentity,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let id = hex::encode(identity.device_cert.device_public_key);
        let encoded = serde_json::to_vec(identity).map_err(err)?;
        let identities = doc
            .get(ROOT, "peer_identities_v1")
            .map_err(err)?
            .ok_or("governed identities missing")?
            .1;
        let names = doc
            .get(ROOT, "peers")
            .map_err(err)?
            .ok_or("governed peers missing")?
            .1;
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        doc.put(&identities, &id, automerge::ScalarValue::Bytes(encoded))
            .map_err(err)?;
        doc.put(&names, &id, identity.device_cert.device_label.as_str())
            .map_err(err)?;
        doc.commit();
        room::media::refresh_authorization(
            self.core.room_doc.as_deref().ok_or("missing room")?,
            &mut doc,
        )
        .map_err(err)?;
        let changes = doc
            .get_changes(&[])
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        let room = self.core.room.clone();
        let outcome = self.receive_from(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp { changes, room },
            now,
        )?;
        Ok(outcome.changed || outcome.metadata_changed)
    }

    fn set_member(
        &mut self,
        user_public_key: [u8; 32],
        role: Role,
        administrator: bool,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.require_governance_writer(key, now)?;
        if user_public_key == self.core.authority.user_public_key {
            return Err("owner role cannot be changed through member governance".into());
        }
        let mut doc = self
            .core
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let policy = doc.policy();
        if !policy.identities.values().any(|bytes| {
            serde_json::from_slice::<GovernedIdentity>(bytes).is_ok_and(|identity| {
                identity.device_cert.user_public_key == user_public_key && identity.has_admission()
            })
        }) {
            return Err("member is absent from governed admission history".into());
        }
        let person = hex::encode(user_public_key);
        let admin_id = administrator_grant_id(user_public_key);
        let before_role = policy.members.get(&person).copied();
        let before_admin = policy.grants.iter().any(|grant| grant.id == admin_id);
        let before = doc.get_heads();
        let members = governed_map(&doc, "members")?;
        let grants = governed_map(&doc, "grants")?;
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        put_member(&mut doc, &members, user_public_key, role)?;
        // An administrator moderates the room too: the two grants come and
        // go together.
        let moderation_id = tilo_action_policy::roles::administrator_moderation_grant_id(&person);
        if administrator {
            let giver = Subject::Person(hex::encode(self.core.authority.user_public_key));
            let room = self.core.room.clone();
            for grant in [
                tilo_action_policy::roles::administrator_grant(&room, &person, giver.clone()),
                tilo_action_policy::roles::administrator_moderation_grant(&room, &person, giver),
            ] {
                doc.put(
                    &grants,
                    grant.id.as_str(),
                    automerge::ScalarValue::Bytes(serde_json::to_vec(&grant).map_err(err)?),
                )
                .map_err(err)?;
            }
        } else {
            for id in [admin_id.as_str(), moderation_id.as_str()] {
                if doc.get(&grants, id).map_err(err)?.is_some() {
                    doc.delete(&grants, id).map_err(err)?;
                }
            }
        }
        doc.commit();
        let changed = self.apply_governance_doc(doc, &before, key, now)?;
        Ok(changed && (before_role != Some(role) || before_admin != administrator))
    }

    fn has_governed_admission(&self, cert: &DeviceCert) -> bool {
        self.core.room_doc.as_ref().is_some_and(|doc| {
            doc.policy().identities.values().any(|bytes| {
                serde_json::from_slice::<GovernedIdentity>(bytes)
                    .is_ok_and(|identity| identity.device_cert == *cert && identity.has_admission())
            })
        })
    }

    fn require_governance_writer(&self, key: &SigningKey, now: SystemTime) -> Result<(), String> {
        if key.verifying_key().to_bytes() != self.core.authority.device_public_key {
            return Err("governance writer is not pinned authority".into());
        }
        self.certificate(&self.core.authority.device_public_key, now)?;
        Ok(())
    }

    fn apply_governance_doc(
        &mut self,
        mut doc: CachedDocument,
        before: &[automerge::ChangeHash],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        room::media::refresh_authorization(
            self.core.room_doc.as_deref().ok_or("missing room")?,
            &mut doc,
        )
        .map_err(err)?;
        let changes = doc
            .get_changes(before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        let room = self.core.room.clone();
        let outcome = self.receive_from(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp { changes, room },
            now,
        )?;
        Ok(outcome.changed || outcome.metadata_changed)
    }
    fn import_governed_certificates(
        &mut self,
        doc: &AutoCommit,
        now: SystemTime,
    ) -> Result<bool, String> {
        let (certificates, revocations, changed) = backend::governed::legacy(
            doc,
            &self.core.certificates,
            &self.core.revocations,
            &self.core.authority,
            now,
        )
        .map_err(err)?;
        self.core.certificates = certificates.into();
        self.core.revocations = revocations;
        Ok(changed)
    }
    /// Verified public identity metadata; never includes signing keys.
    pub fn certificates(&self) -> Vec<DeviceCert> {
        self.core
            .certificates
            .values()
            .map(|cert| cert.as_ref().clone())
            .collect()
    }
    pub fn revocations(&self) -> Vec<tilo_user_identity::RevocationEntry> {
        let mut entries = self.core.revocations.iter().cloned().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.revoked_device_public_key);
        entries
    }
    pub fn replay_floors(&self) -> Vec<(String, String, [u8; 32], u64)> {
        let mut rows = self
            .core
            .documents
            .iter()
            .flat_map(|(route, document)| {
                document
                    .channel
                    .last_seq_per_author
                    .iter()
                    .map(|(author, seq)| {
                        (
                            route.app.clone(),
                            route.record.clone(),
                            *author.as_bytes(),
                            *seq,
                        )
                    })
            })
            .collect::<Vec<_>>();
        rows.sort();
        rows
    }
    pub(crate) fn fresh_checkpoint_for(&self, app: &str, record: &str, now: SystemTime) -> bool {
        self.core
            .checkpoint_times
            .get(&Route {
                app: app.into(),
                record: record.into(),
            })
            .is_some_and(|at| {
                now.duration_since(*at)
                    .is_ok_and(|age| age <= Duration::from_secs(300))
            })
    }
    pub fn authority(&self) -> &DeviceCert {
        &self.core.authority
    }
    pub fn room(&self) -> &str {
        &self.core.room
    }
    /// The current authority of the device `source`: its person, role, the
    /// room's policy and whether it is held to the foreign-device bound.
    ///
    /// # Errors
    ///
    /// Returns an error if `source` has no current certified identity, is not a
    /// current member, or governance is missing.
    pub fn permissions(
        &self,
        source: [u8; 32],
        now: SystemTime,
    ) -> Result<MemberAuthority, String> {
        let (role, agent) = backend::member::legacy(&self.core, &source, now).map_err(err)?;
        let cert = self.certificate(&source, now)?;
        let policy = self
            .core
            .room_doc
            .as_ref()
            .ok_or("room missing")?
            .policy()
            .room_policy(&self.core.room);
        let person = Principal::person_from_key(Ed25519PublicKey::from_bytes(cert.user_public_key));
        Ok(MemberAuthority {
            role,
            person: person.id().to_owned(),
            policy,
            foreign: backend::member::foreign(cert, &self.core.authority),
            delegate: agent,
        })
    }
    /// Committed, admitted room only. Callers cannot mutate the admission owner.
    pub fn admitted_room(&self) -> Option<&AutoCommit> {
        self.core.room_doc.as_deref()
    }

    /// Read permission for an exact canonical record using the same governed
    /// actor/resource vocabulary as document mutation admission.
    ///
    /// # Errors
    ///
    /// Returns an error if `device` may not read the record under current governance.
    pub fn check_document_read(
        &self,
        device: [u8; 32],
        app: &str,
        record: &str,
        now: SystemTime,
    ) -> Result<(), String> {
        let member = self.permissions(device, now)?;
        let bound = tilo_action_policy::roles::foreign_device_bound();
        if !backend::access::document_read(
            &member.authority(&bound),
            &self.core.room,
            app,
            record,
            now,
        ) {
            return Err(format!("record read forbidden: {app}/{record}"));
        }
        Ok(())
    }
    pub fn has_governance(&self) -> bool {
        self.core.room_doc.is_some()
    }
    /// An encrypted record a replica without exact admission evidence could
    /// only mark, not keep. A v5 replica keeps every sealed write, as its
    /// ciphertext or, as the key holder, as the plaintext it verified.
    pub fn incomplete_reason(&self) -> Option<String> {
        (!self.core.admission_v5
            && self
                .core
                .documents
                .values()
                .any(|entry| entry.encryption_required))
        .then(|| {
            "incomplete: authenticated persisted MLS group required; plaintext downgrade forbidden"
                .into()
        })
    }
    /// Validated data from this exact snapshot, not an authorization credential.
    /// Missing exact Chat prerequisites and unpromoted local revocations fail
    /// closed. Current device permissions/time still require separate checks.
    ///
    /// # Errors
    ///
    /// Returns an error if the room media policy fails validation against this snapshot.
    pub fn validated_media_policy(&self) -> Result<Option<room::media::RoomMediaV1>, String> {
        let policy = self.validate_present_media_policy()?;
        if let Some(policy) = &policy {
            backend::media::ready(
                policy,
                |app, record| backend::media::legacy_binding(self, policy, app, record),
                || {
                    room::media::security_basis(
                        self.core.room_doc.as_deref().ok_or("missing room")?,
                        &self.revocations(),
                    )
                    .map_err(err)
                },
            )
            .map_err(backend::media::ReadinessError::legacy)?;
        }
        Ok(policy)
    }
    fn validate_present_media_policy(&self) -> Result<Option<room::media::RoomMediaV1>, String> {
        self.core
            .room_doc
            .as_deref()
            .map(|doc| {
                room::media::validate_transition(None, doc, room::media::TransitionMode::Restore)
                    .map_err(err)
            })
            .transpose()
            .map(Option::flatten)
    }
    fn validate_media_bindings(
        &self,
        policy: &room::media::RoomMediaV1,
        require_complete: bool,
    ) -> Result<(), String> {
        backend::media::bindings(policy, require_complete, |app, record| {
            backend::media::legacy_binding(self, policy, app, record)
        })
        .map_err(backend::media::ReadinessError::legacy)
    }

    pub fn pending_count(&self) -> usize {
        self.core.pending.len()
    }
    /// This only authorizes recovery of an overtaken sequence. It is not
    /// acknowledgement: `covers()` additionally requires causal change inclusion.
    pub(crate) fn checkpoint_sequence_covers(&self, message: &NeighbourMsg) -> bool {
        let NeighbourMsg::AppDocument {
            app_id,
            cluster_id,
            record_id,
            envelope,
            ..
        } = message
        else {
            return false;
        };
        if cluster_id != &self.core.room {
            return false;
        }
        self.core
            .documents
            .get(&Route {
                app: app_id.clone(),
                record: record_id.clone(),
            })
            .and_then(|document| {
                document
                    .covered
                    .get(&hex::encode(envelope.author.as_bytes()))
            })
            .is_some_and(|floor| *floor >= envelope.seq)
    }
    pub fn covers(&self, message: &NeighbourMsg) -> bool {
        let NeighbourMsg::AppDocument {
            app_id,
            cluster_id,
            record_id,
            envelope,
            ..
        } = message
        else {
            return false;
        };
        if cluster_id != &self.core.room {
            return false;
        }
        let Some(document) = self.core.documents.get(&Route {
            app: app_id.clone(),
            record: record_id.clone(),
        }) else {
            return false;
        };
        if document
            .covered
            .get(&hex::encode(envelope.author.as_bytes()))
            .copied()
            .unwrap_or(0)
            < envelope.seq
        {
            return false;
        }
        let Some(snapshot) = &document.covered_snapshot else {
            return false;
        };
        let Ok(collab_types::WholeDocumentPayload::Cleartext(DocumentMutation::Incremental(bytes))) =
            collab_types::validate_whole_document_payload_route(
                app_id, cluster_id, record_id, envelope,
            )
        else {
            return false;
        };
        let Ok(checkpoint) = snapshot.decoded() else {
            return false;
        };
        // A sequence floor is not causal coverage: an out-of-order incremental
        // may have been validated while its dependencies were unavailable.
        // Contained: applied in the isolate to a copy of the checkpoint, the
        // peer's bytes leave its heads where they were and nothing waiting.
        let mut proof = checkpoint.doc.clone();
        matches!(
            tilo_automerge_isolate::apply(
                &mut proof,
                &bytes,
                tilo_automerge_isolate::Dependencies::RequireComplete
            ),
            Ok(tilo_automerge_isolate::Applied::Changed(false))
        )
    }
    pub fn rows(&self) -> Vec<DocumentRow> {
        self.core
            .documents
            .iter()
            .map(|(route, entry)| DocumentRow {
                app_id: route.app.clone(),
                record_id: route.record.clone(),
                heads: entry
                    .doc
                    .base_heads()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            })
            .collect()
    }

    /// Read the admitted document basis without serializing its contents.
    pub fn document_heads(&self, app: &str, record: &str) -> Option<Vec<automerge::ChangeHash>> {
        self.core
            .documents
            .get(&Route {
                app: app.into(),
                record: record.into(),
            })
            .map(|entry| entry.doc.base_heads())
    }

    /// Whether `record`'s governed checkpoint is still arriving: the room
    /// already carries its coverage, but the checkpoint body it names has
    /// not been admitted, so the document's accepted checkpoint is an older
    /// one. A restoring read refuses this room until the body arrives; it is
    /// a cut in flight, not a damaged replica.
    #[must_use]
    pub fn checkpoint_pending(&self, app: &str, record: &str) -> bool {
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        let Some(document) = self.core.documents.get(&route) else {
            return false;
        };
        let snapshot = document
            .covered_snapshot
            .as_ref()
            .map(|snapshot| snapshot.0.bytes.as_slice());
        let room = self
            .core
            .room_doc
            .as_deref()
            .map(crate::room::read::LegacyRead);
        matches!(
            crate::coverage::read::governed_snapshot_matches(
                room.as_ref(),
                app,
                record,
                snapshot,
                &crate::coverage::read::LegacyCoverage,
            ),
            Err(crate::coverage::read::CoverageMatchError::NoMatch)
        )
    }

    /// Heads of a plaintext, causally complete document. Missing, encrypted,
    /// and incomplete routes return `None` so a caller falls through to a
    /// restoring read and keeps that refusal.
    pub fn plaintext_complete_heads(
        &self,
        app: &str,
        record: &str,
    ) -> Option<Vec<automerge::ChangeHash>> {
        let entry = self.core.documents.get(&Route {
            app: app.into(),
            record: record.into(),
        })?;
        if entry.encryption_required || !ReadDoc::get_missing_deps(&*entry.doc, &[]).is_empty() {
            return None;
        }
        Some(entry.doc.base_heads())
    }

    /// The changes a replica holding exactly the heads `since` lacks of a
    /// plaintext, causally complete document, as Automerge change bytes, and
    /// the heads they lead to. Built from the changes since `since` alone,
    /// never from a copy or a save of the whole document. `None` when a
    /// whole read must decide instead: the route is missing, encrypted or
    /// incomplete, a `since` head is not in the document, nothing follows
    /// `since`, or the changes are larger than `maximum` bytes.
    #[must_use]
    pub fn plaintext_changes_since(
        &self,
        app: &str,
        record: &str,
        since: &[automerge::ChangeHash],
        maximum: usize,
    ) -> Option<PlaintextChanges> {
        let entry = self.core.documents.get(&Route {
            app: app.into(),
            record: record.into(),
        })?;
        if entry.encryption_required
            || since.is_empty()
            || !ReadDoc::get_missing_deps(&*entry.doc, &[]).is_empty()
            || !ReadDoc::get_missing_deps(&*entry.doc, since).is_empty()
        {
            return None;
        }
        let document = entry.doc.committed_document();
        let bytes = document.save_after(since);
        if bytes.is_empty() || bytes.len() > maximum {
            return None;
        }
        Some(PlaintextChanges {
            heads: document.get_heads(),
            bytes,
        })
    }
    /// The `(app, record)` of every record this peer holds readable, not
    /// sealed for a channel's members.
    pub fn plaintext_routes(&self) -> Vec<(String, String)> {
        self.core
            .documents
            .iter()
            .filter(|(_, entry)| !entry.encryption_required)
            .map(|(route, _)| (route.app.clone(), route.record.clone()))
            .collect()
    }
    /// Whether this state holds `record` of `app`, without saving it as
    /// [`Self::document`] does.
    #[must_use]
    pub fn holds_document(&self, app: &str, record: &str) -> bool {
        self.core.documents.contains_key(&Route {
            app: app.into(),
            record: record.into(),
        })
    }
    /// The `(app, record)` of every encrypted record this state holds: only
    /// its channel's key holders read it.
    #[must_use]
    pub fn sealed_routes(&self) -> Vec<(String, String)> {
        self.core
            .documents
            .iter()
            .filter(|(_, entry)| entry.encryption_required)
            .map(|(route, _)| (route.app.clone(), route.record.clone()))
            .collect()
    }
    pub fn document(&self, app: &str, record: &str) -> Option<Vec<u8>> {
        self.core
            .documents
            .get(&Route {
                app: app.into(),
                record: record.into(),
            })
            .map(|entry| entry.doc.saved())
    }
    fn certificate(&self, source: &[u8; 32], now: SystemTime) -> Result<&DeviceCert, String> {
        self.core.certificate_typed(source, now).map_err(err)
    }
    fn role(&self, source: &[u8; 32], now: SystemTime) -> Result<Role, String> {
        backend::member::legacy(&self.core, source, now)
            .map(|(role, _)| role)
            .map_err(err)
    }

    /// Decide received changes by their claims against the app surfaces
    /// `surfaces` finds.
    pub fn set_surfaces(&mut self, surfaces: collab_types::Surfaces) {
        self.surfaces = surfaces;
    }

    /// The bot actor `source` may show other names as: a bot device that
    /// acts in the room now (live delegation, live installation, installer
    /// still a member) and whose installation allows `Personas`.
    fn appearance_author(&self, source: &[u8; 32], now: SystemTime) -> Option<String> {
        self.permissions(*source, now).ok()?;
        let doc = self.core.room_doc.as_ref()?;
        let bytes = doc.policy().identities.get(&hex::encode(source))?.clone();
        let identity: GovernedIdentity = serde_json::from_slice(&bytes).ok()?;
        let installation = identity.delegate?.installation?;
        (installation.appearance == crate::installation::AppearancePolicy::Personas)
            .then(|| format!("bot:{}", installation.id))
    }

    /// The authority of `source` for the change `envelope` makes on `record`
    /// of `app_id`, refused when the change is not granted; `None` while
    /// the app surface it is decided against is not here.
    fn authorize_document(
        &self,
        source: &[u8; 32],
        app_id: &str,
        record: &str,
        envelope: &SignedOp,
        authorization_time: SystemTime,
    ) -> Result<Option<MemberAuthority>, String> {
        let member = self.permissions(*source, authorization_time)?;
        let route = Route {
            app: app_id.into(),
            record: record.into(),
        };
        // The record as this peer holds it, read only when changes are
        // decided against app surfaces: which of the app's resources it is,
        // and which units a change writes.
        let document = self
            .surfaces
            .enforced()
            .then(|| self.core.documents.get(&route))
            .flatten();
        let kind = document.and_then(|document| record_kind(&*document.doc));
        let pinned = document.and_then(|document| record_surface(&*document.doc));
        let held = document.and_then(|document| held_units(&document.doc));
        let decided = collab_types::decide_op_pinned(
            envelope,
            &collab_types::Record {
                room: &self.core.room,
                app: app_id,
                record,
            },
            kind.as_deref(),
            held,
            pinned,
            &self.surfaces,
            &|node, kind| member.allows(node, kind, authorization_time),
        );
        match decided {
            Ok(collab_types::Receipt::Admit) => Ok(Some(member)),
            Ok(collab_types::Receipt::Hold) => Ok(None),
            Err(refusal) => Err(format!("document change forbidden: {refusal}")),
        }
    }

    /// Select native schema v5 only as part of an outer owner's reviewed rollout.
    /// This selects candidate serialization; it does not commit any bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the current exact admission index does not fit schema v5.
    pub fn select_admission_v5(&mut self) -> Result<(), AdmissionError> {
        self.core.exact_admissions.encode()?;
        self.core.admission_v5 = true;
        Ok(())
    }
    pub fn admission_v5_selected(&self) -> bool {
        self.core.admission_v5
    }

    /// Exact observations from the real admission path, not a durability receipt.
    ///
    /// # Errors
    ///
    /// Returns an [`AdmissionError`] when the message is refused.
    pub fn receive_from_report(
        &mut self,
        origin: TransportOrigin,
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<AdmissionReport, AdmissionError> {
        if matches!(message, NeighbourMsg::AppDocument { .. }) && !self.core.admission_v5 {
            return Err(AdmissionError::WriterSelectionRequired);
        }
        if let TransportOrigin::RetainedNativePublication { publisher } = origin {
            return self.receive_native_publication(publisher, message, now, true);
        }
        if matches!(&message, NeighbourMsg::AppDocument { .. })
            && let Some(report) = self.lookup_exact_document_report(origin, &message, now)?
        {
            return Ok(report);
        }
        let (source, retained_replay) = self.resolve_origin(origin, &message, now)?;
        let mut candidate = self.clone();
        let report = candidate.receive_with_context(source, message, now, retained_replay, true)?;
        *self = candidate;
        Ok(report)
    }

    /// Authenticate an already recorded document without copying mutable state.
    /// The report observes the existing admission, never a new commit barrier.
    ///
    /// # Errors
    /// Current publisher authority, transport, route or signature is invalid.
    pub fn lookup_exact_document_report(
        &self,
        origin: TransportOrigin,
        message: &NeighbourMsg,
        now: SystemTime,
    ) -> Result<Option<AdmissionReport>, AdmissionError> {
        if !self.core.admission_v5 {
            return Err(AdmissionError::WriterSelectionRequired);
        }
        if let Some(publisher) = self.core.native_cut_publisher {
            self.validate_native_publisher(publisher, now)?;
        }
        let (source, _) = self.resolve_origin(origin, message, now)?;
        if self.core.room_doc.is_none() || !self.core.certificates.contains_key(&source) {
            return Ok(None);
        }
        self.lookup_exact_document(origin, message, now)
            .map(|exact| {
                exact.map(|admitted| {
                    AdmissionReport::new(
                        InputDisposition::Duplicate(admitted),
                        ReceiveOutcome {
                            duplicate: true,
                            ..Default::default()
                        },
                    )
                })
            })
    }

    /// A read-only exact retry lookup. Current authority and signature/route still
    /// apply; only a recorded envelope bypasses new-write timestamp freshness.
    ///
    /// # Errors
    ///
    /// Returns an [`AdmissionError`] if the message is not a valid document envelope for
    /// this room or its author is not currently authorized.
    pub fn lookup_exact_document(
        &self,
        origin: TransportOrigin,
        message: &NeighbourMsg,
        now: SystemTime,
    ) -> Result<Option<DocumentAdmission>, AdmissionError> {
        let (source, _) = self.resolve_origin(origin, message, now)?;
        let NeighbourMsg::AppDocument {
            peer,
            app_id,
            cluster_id,
            record_id,
            envelope,
        } = message
        else {
            return Err("exact document lookup requires AppDocument".into());
        };
        if *peer != peer_handle(&source) {
            return Err("transport author mismatch".into());
        }
        self.lookup_exact_document_parts(source, app_id, cluster_id, record_id, envelope, now)
    }
    fn lookup_exact_document_parts(
        &self,
        source: [u8; 32],
        app: &str,
        room: &str,
        record: &str,
        op: &SignedOp,
        now: SystemTime,
    ) -> Result<Option<DocumentAdmission>, AdmissionError> {
        if room != self.core.room || op.author.as_bytes() != &source {
            return Err("transport author or document room mismatch".into());
        }
        if self
            .authorize_document(&source, app, record, op, now)?
            .is_none()
        {
            return Err("document change held: its app surface is not here yet".into());
        }
        collab_types::validate_whole_document_payload_route(app, room, record, op).map_err(err)?;
        collab_types::verify_op(op).map_err(err)?;
        let admission = DocumentAdmission::from_op(room, app, record, op)?;
        match self.core.exact_admissions.get(&admission.key) {
            Some(id) if id == &admission.operation_id => Ok(Some(admission)),
            Some(_) => Err(AdmissionError::ConflictingSequence),
            // A compaction folded it: decided, and ignored like a retry.
            None if self.core.exact_admissions.covers(&admission.key) => Ok(Some(admission)),
            None => Ok(None),
        }
    }

    /// Exact new candidate admissions for local multi-record owner commits.
    /// Also refuses replacement that discards prior exact evidence.
    ///
    /// # Errors
    ///
    /// Returns an [`AdmissionError`] if this candidate discards exact evidence that
    /// `before` recorded.
    pub fn admissions_since(
        &self,
        before: &StatePeer,
    ) -> Result<Vec<DocumentAdmission>, AdmissionError> {
        if self.core.room != before.core.room
            || self.core.authority != before.core.authority
            || (before.core.admission_v5 && !self.core.admission_v5)
        {
            return Err(
                "admission candidate replaces namespace or downgrades native writer".into(),
            );
        }
        let index = &self.core.exact_admissions;
        let prior = &before.core.exact_admissions;
        // A candidate staged from `before` shares its settled entries and
        // marks until it settles or covers some itself: then both hold
        // the same beyond their recent entries, and only those differ.
        // Walking the whole index made each governance change cost every
        // write the room ever kept.
        if index.shares_settled(prior) {
            for (key, id) in prior.recent() {
                if index.get(key) != Some(id) {
                    return Err("admission candidate discards exact evidence".into());
                }
            }
            return Ok(index
                .recent()
                .filter(|(key, _)| prior.get(key).is_none())
                .map(|(key, id)| DocumentAdmission {
                    key: key.clone(),
                    operation_id: *id,
                })
                .collect());
        }
        // Evidence may only be kept exactly or folded under a mark that a
        // compaction raised; marks never fall.
        for (key, id) in before.core.exact_admissions.iter() {
            if index.get(key) != Some(id) && !index.covers(key) {
                return Err("admission candidate discards exact evidence".into());
            }
        }
        for (writer, mark) in before.core.exact_admissions.marks() {
            if !index.covers(&writer.at(mark)) {
                return Err("admission candidate lowers a compaction mark".into());
            }
        }
        Ok(self
            .core
            .exact_admissions
            .iter()
            .filter(|(key, _)| {
                before.core.exact_admissions.get(key).is_none()
                    && !before.core.exact_admissions.covers(key)
            })
            .map(|(key, id)| DocumentAdmission {
                key: key.clone(),
                operation_id: *id,
            })
            .collect())
    }

    /// Admit a message from `origin`, queuing it while prerequisites are missing.
    ///
    /// # Errors
    ///
    /// Returns an error if the message is refused.
    pub fn receive_from(
        &mut self,
        origin: TransportOrigin,
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<ReceiveOutcome, String> {
        if let TransportOrigin::RetainedNativePublication { publisher } = origin {
            return self
                .receive_native_publication(publisher, message, now, false)
                .map(|report| report.summary)
                .map_err(err);
        }
        let (source, retained_replay) = self.resolve_origin(origin, &message, now).map_err(err)?;
        let mut candidate = self.clone();
        let outcome = candidate
            .receive_with_context(source, message, now, retained_replay, false)
            .map_err(err)?;
        *self = candidate;
        Ok(outcome.summary)
    }
    pub(crate) fn receive(
        &mut self,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<ReceiveOutcome, String> {
        self.receive_with_context(source, message, now, false, false)
            .map(|report| report.summary)
            .map_err(err)
    }
    fn receive_with_context(
        &mut self,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
        retained_replay: bool,
        strict_report: bool,
    ) -> Result<AdmissionReport, AdmissionError> {
        if let Some(publisher) = self.core.native_cut_publisher {
            self.validate_native_publisher(publisher, now)?;
        }
        match message {
            NeighbourMsg::IdentityHello {
                protocol,
                cert,
                revocations,
            } => {
                // A device at another peer protocol version is admitted to
                // nothing: what it says is not read at this one.
                if protocol != neighbour_wire::PEER_PROTOCOL_VERSION {
                    return Err(AdmissionError::Rejected(format!(
                        "the device speaks peer protocol {protocol}, this one {}",
                        neighbour_wire::PEER_PROTOCOL_VERSION
                    )));
                }
                self.receive_identity_hello(source, &cert, revocations, now)
            }
            NeighbourMsg::SyncResp {
                changes,
                room: received,
            } => self.receive_room_changes(source, &received, &changes, now, strict_report),
            message @ NeighbourMsg::AppDocument { .. } => {
                if self.core.room_doc.is_none() || !self.core.certificates.contains_key(&source) {
                    let prerequisite = if self.core.room_doc.is_none() {
                        MissingPrerequisite::Governance
                    } else {
                        MissingPrerequisite::Certificate
                    };
                    return self.defer(source, message, retained_replay, prerequisite);
                }
                self.receive_document(source, message, now, retained_replay, strict_report)
            }
            _ => Ok(AdmissionReport::new(
                InputDisposition::UnsupportedFamily,
                ReceiveOutcome::default(),
            )),
        }
    }
    /// Admit the transport device's own certificate and the revocations its
    /// identity root signed.
    fn receive_identity_hello(
        &mut self,
        source: [u8; 32],
        cert: &DeviceCert,
        revocations: Vec<tilo_user_identity::RevocationEntry>,
        now: SystemTime,
    ) -> Result<AdmissionReport, AdmissionError> {
        if cert.device_public_key != source {
            return Err("transport certificate mismatch".into());
        }
        if self.core.certificates.get(&source).map(AsRef::as_ref) != Some(cert) {
            let admitted = self.core.room_doc.as_ref().and_then(|doc| {
                doc.identities()
                    .get(&hex::encode(source))
                    .and_then(|identity| identity.as_ref().ok())
            });
            if admitted.is_none_or(|identity| identity.device_cert != *cert) {
                return Err("identity is absent from admitted roster".into());
            }
        }
        let verified = VerifiedCertificate::new(cert.clone(), now_ns(now))?;
        let mut metadata_changed =
            self.core.certificates.get(&source).map(AsRef::as_ref) != Some(cert);
        for entry in revocations {
            if entry.user_public_key != cert.user_public_key {
                return Err("revocation root mismatch".into());
            }
            if self
                .core
                .certificates
                .get(&entry.revoked_device_public_key)
                .is_some_and(|target| target.user_public_key != entry.user_public_key)
            {
                return Err("revocation cannot cross identity roots".into());
            }
            metadata_changed |= self.core.revocations.insert(entry).map_err(err)?;
        }
        if self.core.revocations.is_revoked(&source) {
            return Err("device revoked".into());
        }
        self.core.certificates.insert(source, verified);
        Ok(AdmissionReport::metadata(ReceiveOutcome {
            metadata_changed,
            ..Default::default()
        }))
    }
    /// Admit governance changes to the room document, then retry every input
    /// deferred until governance arrived.
    fn receive_room_changes(
        &mut self,
        source: [u8; 32],
        received: &str,
        room_changes: &[Vec<u8>],
        now: SystemTime,
        strict_report: bool,
    ) -> Result<AdmissionReport, AdmissionError> {
        if received != self.core.room {
            return Err("room topic mismatch".into());
        }
        let cert = self.certificate(&source, now)?;
        let person = cert.user_public_key;
        let foreign = backend::member::foreign(cert, &self.core.authority);
        // Bootstrap: the invite's authority device supplies the first
        // governance before any roster exists to resolve it against.
        if self.core.room_doc.is_some() || source != self.core.authority.device_public_key {
            self.role(&source, now)?;
        }
        let pinned = hex::encode(self.core.authority.device_public_key);
        let from = hex::encode(source);
        // The one room admission the shell's workspace tree runs too.
        let room::AdmittedRoom { candidate, media } = room::admit_room_changes(
            room::RoomAdmission {
                before: self.core.room_doc.as_deref(),
                room: &self.core.room,
                source: room::Writer {
                    key: &from,
                    standing: room::Standing::Member { person, foreign },
                },
                authority: room::AuthorityPin::Device(&pinned),
                contents: room::ContentOwnership::Registry,
                caps_now: now_secs(now),
            },
            room_changes,
        )
        .map_err(err)?;
        // Receipts this peer admitted earlier stay immutable even once
        // the room document omits them.
        let mut receipts = self.core.receipts.clone();
        room::admit_receipts(&mut receipts, &candidate).map_err(err)?;
        let changed = self
            .core
            .room_doc
            .as_deref()
            .is_none_or(|doc| doc.base_heads() != candidate.base_heads());
        if let Some(policy) = &media {
            self.validate_media_bindings(policy, false)?;
        }
        let mut metadata_changed = self.import_governed_certificates(&candidate, now)?;
        self.core.receipts = receipts;
        self.core.room_doc = Some(candidate.into());
        let pending = std::mem::take(&mut self.core.pending);
        self.core.pending_bytes = 0;
        let mut applied = changed;
        let mut rejected = 0usize;
        let mut settled = Vec::new();
        for (author, message, replayed) in pending {
            match self.receive_with_context(author, message, now, replayed, strict_report) {
                Ok(outcome) => {
                    applied |= outcome.changed;
                    metadata_changed |= outcome.metadata_changed;
                    rejected = rejected.saturating_add(outcome.rejected);
                    settled.extend(outcome.settled);
                }
                Err(error) => {
                    if self.core.native_cut_publisher.is_some() {
                        return Err(error);
                    }
                    rejected = rejected.saturating_add(1);
                }
            }
        }
        let mut report = AdmissionReport::metadata(ReceiveOutcome {
            changed: applied,
            metadata_changed,
            incomplete: self.incomplete_reason(),
            deferred: !self.core.pending.is_empty(),
            rejected,
            ..Default::default()
        });
        report.settled = settled;
        Ok(report)
    }
    /// True only when governance covers this exact route with a checkpoint by
    /// the same author at a strictly greater sequence than `sequence`. Any other
    /// coverage relation is not a supersession and keeps its normal handling.
    fn superseded_checkpoint(
        &self,
        route: &Route,
        source: &[u8; 32],
        sequence: u64,
    ) -> Result<bool, AdmissionError> {
        let Some(room) = self.core.room_doc.as_deref() else {
            return Ok(false);
        };
        let entries = crate::coverage::entries(room)?;
        Ok(entries
            .get(&crate::coverage::route_key(&route.app, &route.record))
            .is_some_and(|coverage| {
                coverage.room == self.core.room
                    && coverage.app == route.app
                    && coverage.record == route.record
                    && coverage.author == *source
                    && coverage.sequence > sequence
            }))
    }
    /// A superseded checkpoint is settled without being applied, so the
    /// delivery cursor can move past it; nothing from it enters state.
    fn superseded_checkpoint_report() -> AdmissionReport {
        AdmissionReport::new(
            InputDisposition::MetadataOnly,
            ReceiveOutcome {
                duplicate: true,
                ..Default::default()
            },
        )
    }
    fn defer(
        &mut self,
        source: [u8; 32],
        message: NeighbourMsg,
        retained_replay: bool,
        prerequisite: MissingPrerequisite,
    ) -> Result<AdmissionReport, AdmissionError> {
        let bytes = postcard::to_allocvec(&message).map_err(err)?;
        if self.core.pending.iter().any(|(key, prior, replayed)| {
            key == &source && *prior == message && *replayed == retained_replay
        }) {
            return Ok(AdmissionReport::new(
                InputDisposition::RetryLater(prerequisite),
                ReceiveOutcome {
                    deferred: true,
                    ..Default::default()
                },
            ));
        }
        let pending_bytes = self
            .core
            .pending_bytes
            .checked_add(bytes.len())
            .filter(|total| *total <= 4 * 1024 * 1024);
        let Some(pending_bytes) = pending_bytes.filter(|_| self.core.pending.len() < 64) else {
            return Err("app prerequisite queue full; fresh checkpoint required".into());
        };
        self.core.pending_bytes = pending_bytes;
        self.core
            .pending
            .push_back((source, message, retained_replay));
        Ok(AdmissionReport::new(
            InputDisposition::RetryLater(prerequisite),
            ReceiveOutcome {
                deferred: true,
                ..Default::default()
            },
        ))
    }
    fn receive_document(
        &mut self,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
        retained_replay: bool,
        strict_report: bool,
    ) -> Result<AdmissionReport, AdmissionError> {
        let mut outcome =
            self.admit_document(source, message, now, None, retained_replay, strict_report)?;
        if outcome.deferred || self.core.pending.is_empty() {
            return Ok(outcome);
        }
        let pending = std::mem::take(&mut self.core.pending);
        self.core.pending_bytes = 0;
        for (author, pending_message, replayed) in pending {
            match self.receive_with_context(author, pending_message, now, replayed, strict_report) {
                Ok(applied) => {
                    outcome.changed |= applied.changed;
                    outcome.metadata_changed |= applied.metadata_changed;
                    outcome.rejected = outcome.rejected.saturating_add(applied.rejected);
                    outcome.settled.extend(applied.settled);
                }
                Err(error) => {
                    if self.core.native_cut_publisher.is_some() {
                        return Err(error);
                    }
                    outcome.rejected = outcome.rejected.saturating_add(1);
                }
            }
        }
        outcome.deferred = !self.core.pending.is_empty();
        Ok(outcome)
    }
    /// Host-local SDK publication after its certified connection was admitted.
    /// No wire message can select this door. Unlike a retained checkpoint, an
    /// SDK snapshot is an authored mutation of an already-owned exact record.
    /// Certificate, current policy, signature, replay and route checks are shared.
    ///
    /// # Errors
    ///
    /// Returns an error if the envelope fails certificate, policy, signature, replay or
    /// route checks.
    pub fn admit_local_app_document(
        &mut self,
        app: &str,
        record: &str,
        envelope: collab_types::SignedOp,
        now: SystemTime,
    ) -> Result<ReceiveOutcome, String> {
        self.admit_local(app, record, envelope, LocalWrite::Plain, now)
    }

    fn admit_local(
        &mut self,
        app: &str,
        record: &str,
        envelope: collab_types::SignedOp,
        write: LocalWrite<'_>,
        now: SystemTime,
    ) -> Result<ReceiveOutcome, String> {
        if self.document(app, record).is_none() {
            return Err("local app publication cannot create a record owner".into());
        }
        let source = *envelope.author.as_bytes();
        self.admit_document(
            source,
            NeighbourMsg::AppDocument {
                peer: peer_handle(&source),
                app_id: app.into(),
                cluster_id: self.core.room.clone(),
                record_id: record.into(),
                envelope: Box::new(envelope),
            },
            now,
            Some(write),
            false,
            false,
        )
        .map(|report| report.summary)
        .map_err(err)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the single document admission path; each check builds on the previous"
    )]
    fn admit_document(
        &mut self,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
        local: Option<LocalWrite<'_>>,
        retained_replay: bool,
        strict_report: bool,
    ) -> Result<AdmissionReport, AdmissionError> {
        let local_app = local.is_some();
        let NeighbourMsg::AppDocument {
            peer,
            app_id,
            cluster_id,
            record_id,
            envelope,
        } = message
        else {
            return Err("not a document message".into());
        };
        if cluster_id != self.core.room
            || peer != peer_handle(&source)
            || envelope.author.as_bytes() != &source
        {
            return Err("transport author or document room mismatch".into());
        }
        if strict_report
            && let Some(admitted) = self.lookup_exact_document_parts(
                source,
                &app_id,
                &cluster_id,
                &record_id,
                &envelope,
                now,
            )?
        {
            return Ok(AdmissionReport::new(
                InputDisposition::Duplicate(admitted),
                ReceiveOutcome {
                    duplicate: true,
                    ..Default::default()
                },
            ));
        }
        // Only legacy retained replay may validate a first admission historically.
        // Strict exact retries returned above; an index miss is a live new write,
        // including when this call drains a retained pending entry.
        let historical_validation = retained_replay && !strict_report;
        let authorization_time = if historical_validation {
            envelope.timestamp
        } else {
            now
        };
        let Some(member) =
            self.authorize_document(&source, &app_id, &record_id, &envelope, authorization_time)?
        else {
            return self.defer(
                source,
                NeighbourMsg::AppDocument {
                    peer,
                    app_id,
                    cluster_id,
                    record_id,
                    envelope,
                },
                retained_replay,
                MissingPrerequisite::AppSurface,
            );
        };
        let payload = collab_types::validate_whole_document_payload_route(
            &app_id,
            &cluster_id,
            &record_id,
            &envelope,
        )
        .map_err(err)?;
        let route = Route {
            app: app_id.clone(),
            record: record_id,
        };
        let route_existed = self.core.documents.contains_key(&route);
        let mut candidate = self.core.documents.get(&route).cloned().unwrap_or_else(|| {
            Document {
                doc: AutoCommit::new().into(),
                channel: ChannelState::default(),
                encryption_required: false,
                covered: BTreeMap::new(),
                covered_snapshot: None,
                sealed: backend::SealedLog::default(),
            }
            .into()
        });
        candidate
            .channel
            .artifacts
            .insert(envelope.artifact, ArtifactKind::Note);
        candidate
            .channel
            .artifact_apps
            .insert(envelope.artifact, app_id);
        candidate
            .channel
            .artifact_records
            .insert(envelope.artifact, route.record.clone());
        candidate.channel.room.clone_from(&self.core.room);
        let mut validated_channel = candidate.channel.clone();
        validated_channel.grants.clone_from(&member.policy.grants);
        validated_channel.surfaces.clone_from(&self.surfaces);
        let foreign_bound = tilo_action_policy::roles::foreign_device_bound();
        let author = member.authority(&foreign_bound);
        match collab_types::validate_admitted_inbound_op(
            &envelope,
            &author,
            &mut validated_channel,
            authorization_time,
            if historical_validation {
                Duration::ZERO
            } else {
                Duration::from_secs(300)
            },
        ) {
            Ok(()) => {}
            Err(collab_types::ValidationError::ReplaySeq { .. })
                if !local_app
                    && !candidate.encryption_required
                    && matches!(
                        &payload,
                        collab_types::WholeDocumentPayload::Cleartext(DocumentMutation::Snapshot(
                            _
                        ))
                    )
                    && self.superseded_checkpoint(&route, &source, envelope.seq)? =>
            {
                // Signed and authorised, but an older checkpoint than the one
                // governance already covers for this route: never applied.
                return Ok(Self::superseded_checkpoint_report());
            }
            Err(collab_types::ValidationError::ReplaySeq { .. }) => {
                return Ok(AdmissionReport::new(
                    InputDisposition::RetryLater(MissingPrerequisite::ExactHistoricalEvidence),
                    ReceiveOutcome {
                        duplicate: true,
                        ..Default::default()
                    },
                ));
            }
            Err(error) => return Err(err(error).into()),
        }
        if envelope.kind == OpKind::Compaction
            && !matches!(
                payload,
                collab_types::WholeDocumentPayload::Encrypted { .. }
            )
        {
            return Err("a document compaction is only ever sealed".into());
        }
        let mutation = match (payload, local) {
            (collab_types::WholeDocumentPayload::Encrypted { .. }, Some(LocalWrite::Opened(open))) => {
                // Every check on the sealed envelope, its author's signature over
                // the ciphertext included, passed above: only now is it opened,
                // and the plaintext takes the normal admission path below.
                // Keep the same signed ciphertext provenance as a keyless
                // replica. Bounds are checked before opening, and this candidate
                // is installed only after the plaintext is fully admitted.
                Self::retain_sealed(&mut candidate, (*envelope).clone())?;
                let plaintext = open(&envelope)?;
                candidate.encryption_required = true;
                DocumentMutation::decode(&plaintext).map_err(err)?
            }
            (_, Some(LocalWrite::Opened(_))) => {
                return Err("only a sealed write is opened by its key holder".into());
            }
            (payload, _) => match payload {
            collab_types::WholeDocumentPayload::Cleartext(mutation) if !candidate.encryption_required=>mutation,
            collab_types::WholeDocumentPayload::Encrypted { .. } if !self.core.admission_v5 => {
                // A replica without exact admission evidence keeps no sealed
                // history: it only records that the record is encrypted, so a
                // later plaintext write cannot downgrade it.
                candidate.encryption_required = true;
                self.core.documents.insert(route, candidate);
                return Ok(AdmissionReport::new(
                    InputDisposition::RetryLater(MissingPrerequisite::PersistedMls),
                    ReceiveOutcome {
                        metadata_changed: true,
                        incomplete: self.incomplete_reason(),
                        ..Default::default()
                    },
                ));
            }
            collab_types::WholeDocumentPayload::Encrypted { .. } => {
                if !route_existed {
                    return self.defer(
                        source,
                        NeighbourMsg::AppDocument {
                            peer,
                            app_id: route.app,
                            cluster_id,
                            record_id: route.record,
                            envelope,
                        },
                        retained_replay,
                        MissingPrerequisite::RecordCheckpoint,
                    );
                }
                return self.admit_sealed(route, candidate, validated_channel, *envelope);
            }
            collab_types::WholeDocumentPayload::Cleartext(_)=>return Err("incomplete: authenticated persisted MLS group required; plaintext downgrade forbidden".into()),
            },
        };
        candidate.channel = validated_channel;
        if let DocumentMutation::Incremental(bytes) = &mutation {
            let changes = collab_types::parse_changes(bytes).map_err(err)?;
            let shows = self.appearance_author(&source, authorization_time);
            collab_types::check_appearances(&changes, shows.as_deref())
                .map_err(|refusal| format!("document change forbidden: {refusal}"))?;
        }
        if let DocumentMutation::Snapshot(bytes) = &mutation
            && !local_app
        {
            let entries =
                crate::coverage::entries(self.core.room_doc.as_deref().ok_or("missing room")?)?;
            let coverage = entries.get(&crate::coverage::route_key(&route.app, &route.record));
            let Some(coverage) = coverage else {
                return self.defer(
                    source,
                    NeighbourMsg::AppDocument {
                        peer,
                        app_id: route.app,
                        cluster_id,
                        record_id: route.record,
                        envelope,
                    },
                    retained_replay,
                    MissingPrerequisite::CheckpointCoverage,
                );
            };
            if coverage.sequence < envelope.seq {
                return self.defer(
                    source,
                    NeighbourMsg::AppDocument {
                        peer,
                        app_id: route.app,
                        cluster_id,
                        record_id: route.record,
                        envelope,
                    },
                    retained_replay,
                    MissingPrerequisite::CheckpointCoverage,
                );
            }
            if coverage.sequence > envelope.seq
                && self.superseded_checkpoint(&route, &source, envelope.seq)?
            {
                // A strictly newer covered checkpoint by the same author governs
                // this route; skip this one unapplied instead of wedging delivery.
                return Ok(Self::superseded_checkpoint_report());
            }
            let mut snapshot = AutoCommit::new();
            match tilo_automerge_isolate::apply(
                &mut snapshot,
                bytes,
                tilo_automerge_isolate::Dependencies::RequireComplete,
            )
            .map_err(err)?
            {
                tilo_automerge_isolate::Applied::Changed(_) => {}
                tilo_automerge_isolate::Applied::MissingDependencies => {
                    return Err("retained checkpoint snapshot is incomplete".into());
                }
            }
            let heads = snapshot
                .base_heads()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            if coverage.room != self.core.room
                || coverage.app != route.app
                || coverage.record != route.record
                || coverage.author != source
                || coverage.sequence != envelope.seq
                || coverage.payload_hash != *blake3::hash(&envelope.payload).as_bytes()
                || coverage.heads != heads
            {
                return Err("checkpoint does not match governed coverage".into());
            }
            if coverage.watermarks.get(&hex::encode(source)).copied() != Some(envelope.seq) {
                return Err("checkpoint author coverage mismatch".into());
            }
            candidate.covered_snapshot = Some(AcceptedCheckpoint::new(bytes.clone()));
            for (author, sequence) in &coverage.watermarks {
                let key: [u8; 32] = hex::decode(author)
                    .map_err(err)?
                    .try_into()
                    .map_err(|_| "coverage key length")?;
                let covered = candidate.covered.entry(author.clone()).or_default();
                *covered = (*covered).max(*sequence);
                let floor = candidate
                    .channel
                    .last_seq_per_author
                    .entry(Ed25519PublicKey::from_bytes(key))
                    .or_default();
                *floor = (*floor).max(*sequence);
            }
        } else if !route_existed {
            return self.defer(
                source,
                NeighbourMsg::AppDocument {
                    peer,
                    app_id: route.app,
                    cluster_id,
                    record_id: route.record,
                    envelope,
                },
                retained_replay,
                MissingPrerequisite::RecordCheckpoint,
            );
        }
        let changed = match document::merge_committed_with_status(&mut candidate.doc, &mutation)
            .map_err(|e| format!("{e:?}"))?
        {
            document::MergeOutcome::Changed(changed) => changed,
            document::MergeOutcome::MissingDependencies if !local_app => {
                return self.defer(
                    source,
                    NeighbourMsg::AppDocument {
                        peer,
                        app_id: route.app,
                        cluster_id,
                        record_id: route.record,
                        envelope,
                    },
                    retained_replay,
                    MissingPrerequisite::CausalDependencies,
                );
            }
            document::MergeOutcome::MissingDependencies => {
                return Err("local publication has unresolved document dependencies".into());
            }
        };
        if local_app && !candidate.doc.get_missing_deps(&[]).is_empty() {
            return Err("local publication has unresolved document dependencies".into());
        }
        if let Some(room) = self.core.room_doc.as_deref()
            && let Some(policy) = room::media::parse(room).map_err(err)?
        {
            room::media::validate_bound_chat_change(
                &policy,
                &route.app,
                &route.record,
                &*candidate.doc,
            )
            .map_err(err)?;
        }
        let primary = if self.core.admission_v5 {
            let admitted =
                DocumentAdmission::from_op(&self.core.room, &route.app, &route.record, &envelope)?;
            self.core.exact_admissions.insert(&admitted)?;
            InputDisposition::Accepted(admitted)
        } else {
            InputDisposition::MetadataOnly
        };
        self.core.documents.insert(route.clone(), candidate);
        // Pending drains catch rejection and keep the outer candidate. Install
        // checkpoint freshness only after every fallible admission step succeeds.
        if !local_app && matches!(mutation, DocumentMutation::Snapshot(_)) {
            self.core.checkpoint_times.insert(route, now);
        }
        Ok(AdmissionReport::new(
            primary,
            ReceiveOutcome {
                changed,
                metadata_changed: true,
                ..Default::default()
            },
        ))
    }
    /// Keep a sealed write this replica holds no key for: its author, role,
    /// route, signature and sequence were checked above (`validated` is the
    /// record's channel with its sequence), and the ciphertext itself
    /// becomes the record's durable history, never opened here. A verified
    /// compaction replaces exactly the writes it covers.
    fn admit_sealed(
        &mut self,
        route: Route,
        mut candidate: backend::StoredDocument<LegacyNative>,
        validated: ChannelState,
        envelope: SignedOp,
    ) -> Result<AdmissionReport, AdmissionError> {
        let admitted =
            DocumentAdmission::from_op(&self.core.room, &route.app, &route.record, &envelope)?;
        let folded = (envelope.kind == OpKind::Compaction)
            .then(|| sealed_compaction::covered(&envelope))
            .transpose()?;
        Self::retain_sealed(&mut candidate, envelope)?;
        let primary = if self.core.admission_v5 {
            self.core.exact_admissions.insert(&admitted)?;
            // The writes the compaction folded are in its snapshot now:
            // their exact evidence becomes one mark per writer.
            if let Some(folded) = folded {
                self.core
                    .exact_admissions
                    .cover((&self.core.room, &route.app, &route.record), folded)?;
            }
            InputDisposition::Accepted(admitted)
        } else {
            InputDisposition::MetadataOnly
        };
        candidate.channel = validated;
        candidate.encryption_required = true;
        self.core.documents.insert(route, candidate);
        Ok(AdmissionReport::new(
            primary,
            ReceiveOutcome {
                changed: true,
                metadata_changed: true,
                ..Default::default()
            },
        ))
    }
    fn retain_sealed(candidate: &mut Document, envelope: SignedOp) -> Result<(), AdmissionError> {
        let full = || -> AdmissionError {
            "sealed record history is full; a key holder must compact it (StatePeer::compact_sealed_history)"
                .into()
        };
        if envelope.kind == OpKind::Compaction {
            let history: backend::SealedLog = sealed_compaction::supersede(
                &candidate.sealed.to_vec(),
                &candidate.channel.last_seq_per_author,
                &envelope,
            )?
            .into();
            if history.len() > MAX_SEALED_OPS || history.bytes() > MAX_SEALED_BYTES {
                return Err(full());
            }
            candidate.sealed = history;
        } else {
            if candidate.sealed.len() >= MAX_SEALED_OPS
                || candidate
                    .sealed
                    .bytes()
                    .saturating_add(envelope.payload.len())
                    > MAX_SEALED_BYTES
            {
                return Err(full());
            }
            candidate.sealed.push(envelope);
        }
        Ok(())
    }
    /// Reinstall `route`'s stored sealed history, checking every write as it
    /// was admitted: sealed for this exact route, signed by its author, with
    /// exact admission evidence and within the record's watermark and bounds.
    /// A compaction is only ever first and covers none of the later writes.
    fn restore_sealed(&mut self, route: &Route, ops: Vec<SignedOp>) -> Result<(), String> {
        sealed_history::restore(&mut self.core, route, ops)
    }
    pub fn room_message(&self) -> NeighbourMsg {
        let changes = self
            .core
            .room_doc
            .clone()
            .map(|mut doc| {
                doc.get_changes(&[])
                    .into_iter()
                    .map(|change| change.raw_bytes().to_vec())
                    .collect()
            })
            .unwrap_or_default();
        NeighbourMsg::SyncResp {
            changes,
            room: self.core.room.clone(),
        }
    }
    /// Install the governed seed room and documents as the authority.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the room authority or the seed is invalid.
    pub fn install_seed(
        &mut self,
        seed: SeedState,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        if key.verifying_key().to_bytes() != self.core.authority.device_public_key {
            return Err("only pinned authority may initialize fixture state".into());
        }
        self.receive(
            key.verifying_key().to_bytes(),
            NeighbourMsg::SyncResp {
                changes: seed.room_changes,
                room: self.core.room.clone(),
            },
            now,
        )?;
        for document in seed.documents {
            self.publish_document(
                &document.app_id,
                &document.record_id,
                document.bytes,
                key,
                now,
            )?;
        }
        Ok(())
    }
    /// Sign and admit a snapshot of `record`, returning the message to publish.
    ///
    /// # Errors
    ///
    /// Returns an error if the local device may not write the record or admission fails.
    pub fn publish_document(
        &mut self,
        app: &str,
        record: &str,
        bytes: Vec<u8>,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        self.signed_mutation(app, record, DocumentMutation::Snapshot(bytes), key, now)
    }
    /// Sign and admit incremental changes to `record`, returning the message to publish.
    ///
    /// # Errors
    ///
    /// Returns an error if the local device may not write the record or admission fails.
    pub fn publish_incremental(
        &mut self,
        app: &str,
        record: &str,
        bytes: Vec<u8>,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        self.signed_mutation(app, record, DocumentMutation::Incremental(bytes), key, now)
    }
    /// Sign and admit `mutation` of `record`, returning the message to publish.
    ///
    /// # Errors
    ///
    /// Returns an error if the local device may not write the record or admission fails.
    pub fn signed_mutation(
        &mut self,
        app: &str,
        record: &str,
        mutation: DocumentMutation,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        let mut candidate = self.clone();
        let message = candidate.signed_mutation_in_place(app, record, mutation, key, now)?;
        *self = candidate;
        Ok(message)
    }
    fn signed_mutation_in_place(
        &mut self,
        app: &str,
        record: &str,
        mutation: DocumentMutation,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        self.validate_present_media_policy()?;
        let (message, coverage) = self.prepare_signed_mutation(app, record, mutation, key, now)?;
        if self.core.admission_v5 {
            let NeighbourMsg::AppDocument { envelope, .. } = &message else {
                return Err("prepared mutation is not an app document".into());
            };
            let admitted =
                DocumentAdmission::from_op(&self.core.room, app, record, envelope).map_err(err)?;
            // Reserve admission capacity before publishing coverage into live governance.
            self.core.exact_admissions.check(&admitted).map_err(err)?;
        }
        if let Some(coverage) = coverage {
            crate::coverage::put(
                self.core
                    .room_doc
                    .as_deref_mut()
                    .ok_or("missing room governance")?,
                coverage,
            )?;
        }
        self.receive(key.verifying_key().to_bytes(), message.clone(), now)?;
        Ok(message)
    }
    #[expect(
        clippy::needless_pass_by_value,
        reason = "callers hand over freshly built mutations, as `signed_mutation` takes ownership"
    )]
    fn prepare_signed_mutation(
        &self,
        app: &str,
        record: &str,
        mutation: DocumentMutation,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(NeighbourMsg, Option<crate::coverage::Coverage>), String> {
        let author = Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        let seq = self
            .core
            .documents
            .get(&route)
            .and_then(|entry| entry.channel.last_seq_per_author.get(&author))
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("sequence exhausted")?;
        let mut op = SignedOp::new_unsigned(
            mutation.encode().map_err(err)?,
            collab_types::document_artifact_id(app, &self.core.room, record),
            author,
            collab_types::document_attestation_id(app, &self.core.room, record),
            OpKind::Change,
            now,
            seq,
        )
        .with_surface(self.app_surfaces.0.as_ref().and_then(|of| of(app)));
        collab_types::sign_op(&mut op, key).map_err(err)?;
        let message = NeighbourMsg::AppDocument {
            peer: peer_handle(author.as_bytes()),
            app_id: app.into(),
            cluster_id: self.core.room.clone(),
            record_id: record.into(),
            envelope: Box::new(op),
        };
        let coverage = if let DocumentMutation::Snapshot(bytes) = &mutation {
            if author.as_bytes() != &self.core.authority.device_public_key {
                return Err("only pinned governor may publish a retained checkpoint".into());
            }
            let snapshot = AutoCommit::load(bytes).map_err(err)?;
            let mut watermarks = self
                .core
                .documents
                .get(&route)
                .map(|entry| {
                    entry
                        .channel
                        .last_seq_per_author
                        .iter()
                        .map(|(key, value)| (hex::encode(key.as_bytes()), *value))
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default();
            watermarks.insert(hex::encode(author.as_bytes()), seq);
            let NeighbourMsg::AppDocument { envelope, .. } = &message else {
                return Err("prepared mutation is not an app document".into());
            };
            Some(crate::coverage::Coverage {
                room: self.core.room.clone(),
                app: app.into(),
                record: record.into(),
                payload_hash: *blake3::hash(&envelope.payload).as_bytes(),
                heads: snapshot
                    .base_heads()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                author: *author.as_bytes(),
                sequence: seq,
                watermarks,
            })
        } else {
            None
        };
        Ok((message, coverage))
    }
    /// An encrypted record this replica keeps as sealed history: no governed
    /// snapshot can carry it, since a cleartext checkpoint would downgrade it.
    fn sealed_route(&self, route: &Route) -> bool {
        self.core
            .documents
            .get(route)
            .is_some_and(|document| document.encryption_required)
    }
    /// The governor's receipt for `route`'s sealed history: its last
    /// checkpoint coverage in `rows`, acknowledging every sealed write this
    /// replica durably holds through the record's sequence watermarks.
    /// `None` for a record without checkpoint coverage.
    fn sealed_receipt(
        &self,
        route: &Route,
        rows: &BTreeMap<String, crate::coverage::Coverage>,
        generation: u64,
    ) -> Result<Option<crate::durability::DurableReceipt>, String> {
        let Some(mut coverage) = rows
            .get(&crate::coverage::route_key(&route.app, &route.record))
            .cloned()
        else {
            return Ok(None);
        };
        let document = self
            .core
            .documents
            .get(route)
            .ok_or("sealed receipt for a record this state does not hold")?;
        for (author, sequence) in &document.channel.last_seq_per_author {
            let watermark = coverage
                .watermarks
                .entry(hex::encode(author.as_bytes()))
                .or_default();
            *watermark = (*watermark).max(*sequence);
        }
        crate::durability::DurableReceipt::new(coverage, generation).map(Some)
    }
    fn signed_snapshots_in_place(
        &mut self,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        self.signed_snapshots_with_receipts(key, now, None)
    }
    /// Publish exact checkpoints of every document. With `receipts_at`, the
    /// same room change also records a durable receipt at that generation for
    /// every covered route (one change per durable bootstrap).
    fn signed_snapshots_with_receipts(
        &mut self,
        key: &SigningKey,
        now: SystemTime,
        receipts_at: Option<u64>,
    ) -> Result<Vec<NeighbourMsg>, String> {
        self.validate_present_media_policy()?;
        let prepared = self
            .core
            .documents
            .iter()
            .filter(|(route, _)| !self.sealed_route(route))
            .map(|(route, entry)| {
                self.prepare_signed_mutation(
                    &route.app,
                    &route.record,
                    DocumentMutation::Snapshot(entry.doc.saved()),
                    key,
                    now,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (messages, coverages): (Vec<_>, Vec<_>) = prepared.into_iter().unzip();
        let coverages = coverages.into_iter().flatten().collect::<Vec<_>>();
        let sealed = self
            .core
            .documents
            .keys()
            .filter(|route| self.sealed_route(route))
            .map(|route| (crate::coverage::route_key(&route.app, &route.record), route))
            .collect::<BTreeMap<_, _>>();
        let mut sealed_receipts = BTreeMap::new();
        if let Some(generation) = receipts_at {
            let rows = crate::coverage::entries(
                self.core
                    .room_doc
                    .as_deref()
                    .ok_or("missing room governance")?,
            )?;
            for (route_key, route) in &sealed {
                if let Some(receipt) = self.sealed_receipt(route, &rows, generation)? {
                    sealed_receipts.insert(route_key.clone(), receipt);
                }
            }
        }
        let room = self
            .core
            .room_doc
            .as_deref_mut()
            .ok_or("missing room governance")?;
        match receipts_at {
            Some(generation) => crate::coverage::put_all_with_receipts(room, coverages, |rows| {
                let mut receipts = sealed_receipts;
                for (route, coverage) in rows {
                    if !sealed.contains_key(route) {
                        receipts.insert(
                            route.clone(),
                            crate::durability::DurableReceipt::new(coverage.clone(), generation)?,
                        );
                    }
                }
                Ok(receipts)
            })?,
            None if !coverages.is_empty() => crate::coverage::put_all(room, coverages)?,
            None => {}
        }
        for message in &messages {
            self.receive(key.verifying_key().to_bytes(), message.clone(), now)?;
        }
        Ok(messages)
    }
    /// Signed snapshots of every admitted document.
    ///
    /// # Errors
    ///
    /// Returns an error if a snapshot cannot be signed or encoded.
    pub fn signed_snapshots(
        &mut self,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let mut candidate = self.clone();
        let messages = candidate.signed_snapshots_in_place(key, now)?;
        *self = candidate;
        Ok(messages)
    }
    /// The governor publishes one consistent room coverage/checkpoint pair.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governor or a checkpoint cannot be produced.
    pub fn bootstrap_messages(
        &mut self,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let mut candidate = self.clone();
        let snapshots = candidate.signed_snapshots_in_place(key, now)?;
        let mut messages = vec![candidate.room_message()];
        messages.extend(snapshots);
        messages.extend(candidate.own_sealed_publications(None));
        *self = candidate;
        Ok(messages)
    }
    /// Prepare, but do not publish, a candidate bound to the next native commit.
    /// The caller must commit this entire state and outgoing batch before exposing it.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governor or a checkpoint cannot be produced.
    pub fn durable_bootstrap_messages(
        &mut self,
        key: &SigningKey,
        now: SystemTime,
        generation: u64,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let mut candidate = self.clone();
        let snapshots = candidate.signed_snapshots_with_receipts(key, now, Some(generation))?;
        let room = candidate
            .core
            .room_doc
            .as_deref_mut()
            .ok_or("missing room governance")?;
        crate::durability::monotonic(self.core.room_doc.as_deref(), room)?;
        let mut messages = vec![candidate.room_message()];
        messages.extend(snapshots);
        messages.extend(candidate.own_sealed_publications(None));
        *self = candidate;
        Ok(messages)
    }
    /// Publish exact checkpoints of the `dirty` routes and, in the same room
    /// change, their coverage plus the complete durable receipt set
    /// (`receipts` updated for each dirty route at `generation`). Equivalent
    /// to one `publish_document` per route followed by a receipt write, but
    /// one room change per durable commit and no interim receipt state.
    fn publish_snapshots_with_receipts(
        &mut self,
        dirty: &[(Route, Vec<u8>)],
        mut receipts: BTreeMap<String, crate::durability::DurableReceipt>,
        generation: u64,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        self.validate_present_media_policy()?;
        let mut messages = Vec::with_capacity(dirty.len());
        let mut coverages = Vec::with_capacity(dirty.len());
        let mut sealed = Vec::new();
        for (route, bytes) in dirty {
            if self.sealed_route(route) {
                sealed.push(route.clone());
                continue;
            }
            let (message, coverage) = self.prepare_signed_mutation(
                &route.app,
                &route.record,
                DocumentMutation::Snapshot(bytes.clone()),
                key,
                now,
            )?;
            if self.core.admission_v5 {
                let NeighbourMsg::AppDocument { envelope, .. } = &message else {
                    return Err("prepared mutation is not an app document".into());
                };
                let admitted = DocumentAdmission::from_op(
                    &self.core.room,
                    &route.app,
                    &route.record,
                    envelope,
                )
                .map_err(err)?;
                // Reserve admission capacity before publishing coverage.
                self.core.exact_admissions.check(&admitted).map_err(err)?;
            }
            coverages.push(coverage.ok_or("missing published coverage")?);
            messages.push(message);
        }
        let covered = crate::coverage::entries(
            self.core
                .room_doc
                .as_deref()
                .ok_or("missing room governance")?,
        )?;
        let mut sealed_receipts = BTreeMap::new();
        for route in &sealed {
            if let Some(receipt) = self.sealed_receipt(route, &covered, generation)? {
                sealed_receipts.insert(
                    crate::coverage::route_key(&route.app, &route.record),
                    receipt,
                );
            }
        }
        crate::coverage::put_all_with_receipts(
            self.core
                .room_doc
                .as_deref_mut()
                .ok_or("missing room governance")?,
            coverages,
            |rows| {
                receipts.extend(sealed_receipts);
                for (route, _) in dirty {
                    let route_key = crate::coverage::route_key(&route.app, &route.record);
                    if sealed.contains(route) {
                        continue;
                    }
                    let covered = rows.get(&route_key).ok_or("missing published coverage")?;
                    receipts.insert(
                        route_key,
                        crate::durability::DurableReceipt::new(covered.clone(), generation)?,
                    );
                }
                Ok(receipts)
            },
        )?;
        for message in &messages {
            self.receive(key.verifying_key().to_bytes(), message.clone(), now)?;
        }
        Ok(messages)
    }
    /// Prepare route-selective publication against the last committed peer.
    /// `SyncResp` here contains only room changes since `before` and MUST be retained
    /// as a delta. Application messages remain exact covered snapshots. The caller
    /// must durably commit the candidate and its baseline plus this tail before send.
    ///
    /// # Errors
    ///
    /// Returns an error if this candidate discards evidence `before` recorded, the
    /// local device lacks the pinned authority or permission, or encryption state is
    /// incomplete.
    #[expect(
        clippy::too_many_lines,
        reason = "one candidate publication built against a single committed baseline"
    )]
    pub fn durable_mutation_messages(
        &mut self,
        before: &StatePeer,
        key: &SigningKey,
        now: SystemTime,
        generation: u64,
    ) -> Result<Vec<NeighbourMsg>, String> {
        self.admissions_since(before).map_err(err)?;
        if self.core.room != before.core.room || self.core.authority != before.core.authority {
            return Err("publication cannot replace the session namespace".into());
        }
        if key.verifying_key().to_bytes() != self.core.authority.device_public_key {
            return Err("publication requires pinned authority".into());
        }
        self.permissions(key.verifying_key().to_bytes(), now)?;
        if self.incomplete_reason().is_some() {
            return Err("cannot publish incomplete encryption state".into());
        }
        let before_room = before
            .core
            .room_doc
            .as_ref()
            .ok_or("missing prior governance")?;
        let mut candidate = self.clone();
        for (route, prior) in &before.core.documents {
            let document = candidate
                .core
                .documents
                .get(route)
                .ok_or("publication discards a record")?;
            // A record still shared with `before` discards nothing.
            if backend::StoredDocument::ptr_eq(prior, document) {
                continue;
            }
            if prior
                .doc
                .base_heads()
                .iter()
                .any(|head| document.doc.get_change_by_hash(head).is_none())
                || prior
                    .channel
                    .last_seq_per_author
                    .iter()
                    .any(|(author, seq)| {
                        document
                            .channel
                            .last_seq_per_author
                            .get(author)
                            .copied()
                            .unwrap_or(0)
                            < *seq
                    })
            {
                return Err("publication discards document history or replay floors".into());
            }
        }
        let room = candidate
            .core
            .room_doc
            .as_mut()
            .ok_or("missing room governance")?;
        let before_heads = before_room.base_heads();
        if before_heads
            .iter()
            .any(|head| room.get_change_by_hash(head).is_none())
        {
            return Err("publication discards room history".into());
        }
        crate::coverage::monotonic(Some(before_room), room)?;
        crate::durability::monotonic(Some(before_room), room)?;
        let old_coverage = crate::coverage::entries(before_room)?;
        let coverage = crate::coverage::entries(room)?;
        let old_receipts = crate::durability::entries(before_room)?;
        let receipts = crate::durability::entries(room)?;
        let mut dirty = Vec::new();
        for (route, document) in &candidate.core.documents {
            let route_key = crate::coverage::route_key(&route.app, &route.record);
            let changed = before.core.documents.get(route).is_none_or(|prior| {
                // A record this change did not touch is still shared.
                let same = backend::StoredDocument::ptr_eq(prior, document);
                (!same
                    && (prior.doc.base_heads() != document.doc.base_heads()
                        || prior.channel.last_seq_per_author
                            != document.channel.last_seq_per_author
                        || prior.encryption_required != document.encryption_required
                        || prior.covered != document.covered
                        || prior.covered_snapshot != document.covered_snapshot))
                    || old_coverage.get(&route_key) != coverage.get(&route_key)
                    || old_receipts.get(&route_key) != receipts.get(&route_key)
            });
            if changed {
                dirty.push((route.clone(), document.doc.saved()));
            }
        }
        let snapshots = if dirty.is_empty() {
            Vec::new()
        } else {
            candidate.publish_snapshots_with_receipts(&dirty, receipts, generation, key, now)?
        };
        let room = candidate
            .core
            .room_doc
            .as_mut()
            .ok_or("missing room governance")?;
        crate::coverage::monotonic(Some(before_room), room)?;
        crate::durability::monotonic(Some(before_room), room)?;
        let changes = room
            .get_changes(&before_heads)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect::<Vec<_>>();
        let mut messages = Vec::new();
        if !changes.is_empty() {
            messages.push(NeighbourMsg::SyncResp {
                changes,
                room: self.core.room.clone(),
            });
        }
        messages.extend(snapshots);
        messages.extend(candidate.own_sealed_publications(Some(before)));
        *self = candidate;
        Ok(messages)
    }
    /// A governed assertion alone is insufficient: bind it to the accepted exact checkpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if the governed coverage does not match the accepted checkpoint.
    pub fn durable_coverage(
        &self,
        app: &str,
        record: &str,
    ) -> Result<Option<crate::durability::DurableReceipt>, String> {
        Ok(self
            .durable_receipt_checkpoint(app, record)?
            .map(|(receipt, _)| receipt))
    }
    fn durable_receipt_checkpoint(
        &self,
        app: &str,
        record: &str,
    ) -> Result<
        Option<(
            crate::durability::DurableReceipt,
            std::sync::Arc<DecodedCheckpoint>,
        )>,
        String,
    > {
        let Some(room) = self.core.room_doc.as_deref() else {
            return Ok(None);
        };
        let Some(receipt) = self.durable_receipt(app, record)? else {
            return Ok(None);
        };
        let coverage = &receipt.coverage;
        let Some(document) = self.core.documents.get(&Route {
            app: app.into(),
            record: record.into(),
        }) else {
            return Ok(None);
        };
        if document.encryption_required {
            return Ok(None);
        }
        let Some(snapshot) = document.covered_snapshot.as_ref() else {
            return Ok(None);
        };
        if crate::coverage::entries(room)?.get(&crate::coverage::route_key(app, record))
            != Some(coverage)
        {
            return Ok(None);
        }
        let accepted = snapshot.decoded()?;
        if accepted.heads != coverage.heads
            || accepted.payload_hash != coverage.payload_hash
            || coverage
                .watermarks
                .iter()
                .any(|(author, seq)| document.covered.get(author).copied().unwrap_or(0) < *seq)
        {
            return Ok(None);
        }
        Ok(Some((receipt, accepted)))
    }
    /// The pinned governor's durable receipt for a route, as governance holds it.
    fn durable_receipt(
        &self,
        app: &str,
        record: &str,
    ) -> Result<Option<crate::durability::DurableReceipt>, String> {
        let Some(room) = self.core.room_doc.as_deref() else {
            return Ok(None);
        };
        let Some(receipt) =
            crate::durability::entries(room)?.remove(&crate::coverage::route_key(app, record))
        else {
            return Ok(None);
        };
        let coverage = &receipt.coverage;
        if coverage.room != self.core.room
            || coverage.app != app
            || coverage.record != record
            || coverage.author != self.core.authority.device_public_key
            || self.core.revocations.is_revoked(&coverage.author)
        {
            return Err("durable receipt authority/route mismatch".into());
        }
        Ok(Some(receipt))
    }
    /// Validate the receipt and current-head coverage against one decoded checkpoint.
    /// All authority, route, payload and watermark checks remain fresh per query.
    ///
    /// # Errors
    ///
    /// Returns an error if the receipt or checkpoint fails validation.
    #[cfg(test)]
    pub fn durable_coverage_with_current(
        &self,
        app: &str,
        record: &str,
    ) -> Result<Option<(crate::durability::DurableReceipt, bool)>, String> {
        let Some((receipt, accepted)) = self.durable_receipt_checkpoint(app, record)? else {
            return Ok(None);
        };
        let document = self
            .core
            .documents
            .get(&Route {
                app: app.into(),
                record: record.into(),
            })
            .ok_or("missing record")?;
        let current = document
            .doc
            .base_heads()
            .iter()
            .all(|head| accepted.doc.get_change_by_hash(head).is_some());
        Ok(Some((receipt, current)))
    }
    /// Whether the durable receipt for the record covers its current heads.
    ///
    /// # Errors
    ///
    /// Returns an error if the receipt or checkpoint fails validation.
    #[cfg(test)]
    pub fn durable_covers_current(&self, app: &str, record: &str) -> Result<bool, String> {
        Ok(self
            .durable_coverage_with_current(app, record)?
            .is_some_and(|(_, current)| current))
    }
    /// Encode this peer's durable state.
    ///
    /// # Errors
    ///
    /// Returns an error if the state cannot be encoded within its limits.
    pub fn durable(&self) -> Result<Vec<u8>, String> {
        let parts = self.durable_parts()?;
        let mut bytes = Vec::with_capacity(parts.iter().map(|part| part.len()).sum());
        for part in &parts {
            bytes.extend_from_slice(part);
        }
        Ok(bytes)
    }

    /// [`Self::durable`] in parts, in order: concatenated, they are its
    /// bytes, and [`Self::restore`] takes them so.
    ///
    /// A room's history is most of its state and hardly changes per
    /// governance change, yet re-encoding all of it made every change cost
    /// the whole history. So each record's part is encoded once per state of
    /// the record, and the exact admission index's settled entries once per
    /// settling: a part nothing changed since an earlier call is the same
    /// allocation as then, and whoever stores the parts apart rewrites only
    /// what changed.
    ///
    /// # Errors
    ///
    /// Returns an error if the state cannot be encoded within its limits.
    pub fn durable_parts(&self) -> Result<Vec<std::sync::Arc<[u8]>>, String> {
        fn put<T: Serialize + ?Sized>(value: &T, out: Vec<u8>) -> Result<Vec<u8>, String> {
            postcard::to_extend(value, out).map_err(err)
        }
        let sealed = self
            .core
            .documents
            .values()
            .filter(|entry| !entry.sealed.is_empty())
            .count();
        if sealed > 0 && !self.core.admission_v5 {
            return Err("sealed record history requires durable admission v5".into());
        }
        let mut documents = Vec::with_capacity(self.core.documents.len());
        for (route, entry) in &self.core.documents {
            documents.push(entry.encoded(route, || {
                let document = postcard::to_allocvec(&DurableDocumentRef {
                    route,
                    bytes: entry.doc.saved_bytes(),
                    watermarks: entry
                        .channel
                        .last_seq_per_author
                        .iter()
                        .map(|(key, seq)| (*key.as_bytes(), *seq))
                        .collect(),
                    encryption_required: entry.encryption_required,
                    covered: &entry.covered,
                    covered_snapshot: entry
                        .covered_snapshot
                        .as_ref()
                        .map(|snapshot| snapshot.0.bytes.as_slice()),
                })
                .map_err(err)?;
                let sealed = if entry.sealed.is_empty() {
                    None
                } else {
                    Some(entry.sealed.encode_into(route, Vec::new())?.into())
                };
                Ok(backend::EncodedDocument {
                    route: route.clone(),
                    document: document.into(),
                    sealed,
                })
            })?);
        }
        // The fields of `DurableV6`/`DurableV5`/`DurableV4` in order: a
        // postcard struct is its fields' encodings one after the other.
        let mut parts: Vec<std::sync::Arc<[u8]>> = Vec::new();
        let mut head = Vec::new();
        if sealed > 0 {
            head = put(&6u32, head)?;
        } else if self.core.admission_v5 {
            head = put(&5u32, head)?;
        }
        head = put(&4u32, head)?;
        head = put(
            &(
                3u32,
                &self.core.room,
                &self.core.authority,
                self.core.room_doc.as_ref().map(CachedDocument::saved_bytes),
                documents.len(),
            ),
            head,
        )?;
        parts.push(head.into());
        parts.extend(documents.iter().map(|encoded| encoded.document.clone()));
        let mut revocations = self.core.revocations.iter().collect::<Vec<_>>();
        revocations.sort_by_key(|entry| entry.revoked_device_public_key);
        let certificates = self
            .core
            .certificates
            .values()
            .map(AsRef::as_ref)
            .collect::<Vec<&DeviceCert>>();
        let mut tail = put(
            &(
                &*self.core.receipts,
                certificates,
                revocations,
                &self.core.pending,
                self.core.pending_bytes,
            ),
            Vec::new(),
        )?;
        if self.core.admission_v5 {
            let (settled, rest) = self.core.exact_admissions.encode_parts().map_err(err)?;
            tail = put(&settled.len().saturating_add(rest.len()), tail)?;
            parts.push(tail.into());
            parts.push(settled);
            tail = rest;
        }
        if sealed > 0 {
            tail = put(&sealed, tail)?;
        }
        parts.push(tail.into());
        parts.extend(
            documents
                .iter()
                .filter_map(|encoded| encoded.sealed.clone()),
        );
        Ok(parts)
    }

    /// The durable encoding as one `postcard` value, made whole each time:
    /// what [`Self::durable_parts`] must equal.
    #[cfg(test)]
    pub(crate) fn durable_whole(&self) -> Result<Vec<u8>, String> {
        let documents = self
            .core
            .documents
            .iter()
            .map(|(route, entry)| DurableDocument {
                route: route.clone(),
                bytes: entry.doc.saved(),
                watermarks: entry
                    .channel
                    .last_seq_per_author
                    .iter()
                    .map(|(key, seq)| (*key.as_bytes(), *seq))
                    .collect(),
                encryption_required: entry.encryption_required,
                covered: entry.covered.clone(),
                covered_snapshot: entry
                    .covered_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.0.bytes.clone()),
            })
            .collect();
        let value = DurableV4 {
            version: 4,
            base: Durable {
                version: 3,
                room: self.core.room.clone(),
                authority: self.core.authority.clone(),
                room_bytes: self.core.room_doc.as_ref().map(CachedDocument::saved),
                documents,
                receipts: (*self.core.receipts).clone(),
                certificates: self
                    .core
                    .certificates
                    .values()
                    .map(|cert| cert.as_ref().clone())
                    .collect(),
                revocations: {
                    let mut entries = self.core.revocations.iter().cloned().collect::<Vec<_>>();
                    entries.sort_by_key(|entry| entry.revoked_device_public_key);
                    entries
                },
            },
            pending: self.core.pending.clone(),
            pending_bytes: self.core.pending_bytes,
        };
        let sealed: Vec<_> = self
            .core
            .documents
            .iter()
            .filter(|(_, entry)| !entry.sealed.is_empty())
            .map(|(route, entry)| (route.clone(), entry.sealed.to_vec()))
            .collect();
        if self.core.admission_v5 && !sealed.is_empty() {
            let exact = self.core.exact_admissions.encode().map_err(err)?;
            postcard::to_allocvec(&DurableV6 {
                version: 6,
                base: value,
                exact: &exact,
                sealed,
            })
            .map_err(err)
        } else if !sealed.is_empty() {
            Err("sealed record history requires durable admission v5".into())
        } else if self.core.admission_v5 {
            let exact = self.core.exact_admissions.encode().map_err(err)?;
            postcard::to_allocvec(&DurableV5 {
                version: 5,
                base: value,
                exact: &exact,
            })
            .map_err(err)
        } else {
            postcard::to_allocvec(&value).map_err(err)
        }
    }

    /// Restore a peer from durable state produced by [`StatePeer::durable`].
    ///
    /// # Errors
    ///
    /// Returns an error if the bytes are malformed, the room or authority does not match,
    /// or restored state fails admission.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "public API: callers hand over the pinned revocation list"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "one durable restore across every stored schema version"
    )]
    pub fn restore(
        bytes: &[u8],
        room: &str,
        authority: DeviceCert,
        roster: Vec<DeviceCert>,
        revocations: RevocationList,
        now: SystemTime,
    ) -> Result<Self, String> {
        let (version, _) = postcard::take_from_bytes::<u32>(bytes).map_err(err)?;
        let mut exact_admissions = ExactAdmissions::default();
        let mut sealed = Vec::new();
        let (durable, pending, pending_bytes) = match version {
            4 => {
                let value: DurableV4 = postcard::from_bytes(bytes).map_err(err)?;
                (value.base, value.pending, value.pending_bytes)
            }
            5 => {
                let (value, tail): (DurableV5<'_>, _) =
                    postcard::take_from_bytes(bytes).map_err(err)?;
                if !tail.is_empty() || value.version != 5 || value.base.version != 4 {
                    return Err("invalid native v5 durable envelope".into());
                }
                exact_admissions = ExactAdmissions::decode(value.exact).map_err(err)?;
                (
                    value.base.base,
                    value.base.pending,
                    value.base.pending_bytes,
                )
            }
            6 => {
                let (value, tail): (DurableV6<'_>, _) =
                    postcard::take_from_bytes(bytes).map_err(err)?;
                if !tail.is_empty()
                    || value.version != 6
                    || value.base.version != 4
                    || value.sealed.is_empty()
                {
                    return Err("invalid native v6 durable envelope".into());
                }
                exact_admissions = ExactAdmissions::decode(value.exact).map_err(err)?;
                sealed = value.sealed;
                (
                    value.base.base,
                    value.base.pending,
                    value.base.pending_bytes,
                )
            }
            _ => return Err("unsupported durable admission version".into()),
        };
        if durable.version != 3 || durable.room != room || durable.authority != authority {
            return Err("durable admission namespace mismatch".into());
        }
        let canonical_room = uuid::Uuid::parse_str(room).map_err(err)?.to_string();
        let verified_certificates = backend::restore_certificate_roster(
            durable.certificates,
            roster,
            &authority,
            now_ns(now),
        )
        .map_err(err)?;
        let mut peer = Self {
            core: PeerCore {
                room: canonical_room,
                authority,
                certificates: verified_certificates.into(),
                revocations: RevocationList::new(),
                receipts: Shared::default(),
                room_doc: None,
                documents: Shared::default(),
                pending,
                pending_bytes,
                checkpoint_times: Shared::default(),
                exact_admissions,
                admission_v5: version >= 5,
                native_cut_publisher: None,
                _retention: (),
            },
            surfaces: process_default().0,
            app_surfaces: process_default().1,
        };
        backend::validate_pending(&peer.core.pending, peer.core.pending_bytes).map_err(err)?;
        peer.core.revocations = backend::restore_revocation_union(
            durable.revocations,
            &revocations,
            &peer.core.certificates,
            peer.core.authority.device_public_key,
        )
        .map_err(err)?;
        peer.core.receipts = durable.receipts.into();
        peer.core.room_doc = durable
            .room_bytes
            .map(|bytes| {
                // Isolated load leaves the document in a worker so the first
                // inbound change does not pay a full rebuild.
                tilo_automerge_isolate::load(&bytes)
                    .map(CachedDocument::from)
                    .map_err(err)
            })
            .transpose()?;
        for document in durable.documents {
            check_governed_snapshot(
                peer.core.room_doc.as_deref(),
                &document.route,
                document.covered_snapshot.as_deref(),
            )?;
            let channel = ChannelState {
                last_seq_per_author: document
                    .watermarks
                    .into_iter()
                    .map(|(key, seq)| (Ed25519PublicKey::from_bytes(key), seq))
                    .collect(),
                ..ChannelState::default()
            };
            peer.core.documents.insert(
                document.route,
                Document {
                    doc: tilo_automerge_isolate::load(&document.bytes)
                        .map(CachedDocument::from)
                        .map_err(err)?,
                    channel,
                    encryption_required: document.encryption_required,
                    covered: document.covered,
                    covered_snapshot: document.covered_snapshot.map(AcceptedCheckpoint::new),
                    sealed: backend::SealedLog::default(),
                }
                .into(),
            );
        }
        for (route, ops) in sealed {
            peer.restore_sealed(&route, ops)?;
        }
        if let Some(policy) = peer.validate_present_media_policy()? {
            let doc = peer.core.room_doc.as_deref().ok_or("missing room")?.clone();
            // Durable identity metadata is not a substitute for matching the
            // governed certificate rows. Reuse the normal verified import and
            // retain historical certificates already validated during restore.
            peer.import_governed_certificates(&doc, now)?;
            if peer
                .core
                .revocations
                .is_revoked(&peer.core.authority.device_public_key)
            {
                return Err("pinned authority device is revoked by governed policy".into());
            }
            // Room-first admission is durable even before its exact Chat
            // checkpoint arrives. Validate present bindings here; the media
            // projection still requires every active prerequisite to be ready.
            peer.validate_media_bindings(&policy, false)?;
        }
        // Every exact entry and every compaction mark lies within its
        // route's watermark.
        let index = &peer.core.exact_admissions;
        let keys: Vec<_> = index
            .iter()
            .map(|(key, _)| key.clone())
            .chain(index.marks().map(|(writer, mark)| writer.at(mark)))
            .collect();
        for key in &keys {
            let floor = peer
                .core
                .documents
                .get(&Route {
                    app: key.app.clone(),
                    record: key.record.clone(),
                })
                .and_then(|document| {
                    document
                        .channel
                        .last_seq_per_author
                        .get(&Ed25519PublicKey::from_bytes(key.author))
                })
                .copied();
            if key.room != peer.core.room || floor.is_none_or(|floor| key.sequence > floor) {
                return Err("exact admission evidence exceeds restored route/floor".into());
            }
        }
        Ok(peer)
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct GovernedIdentity {
    pub(crate) device_cert: DeviceCert,
    #[serde(default)]
    pub(crate) revocations: Vec<tilo_user_identity::RevocationEntry>,
    #[serde(default)]
    pub(crate) admission: Option<RedemptionReceipt>,
    #[serde(default)]
    pub(crate) admissions: Vec<RedemptionReceipt>,
    #[serde(default)]
    pub(crate) workspace_delegations: Vec<crate::join::WorkspaceDelegation>,
    /// Present when the device is a delegate of its person: an agent, or an
    /// installed bot ([`delegates`]). Absent from every other identity's
    /// bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) delegate: Option<GovernedDelegateRow>,
}
impl GovernedIdentity {
    pub(crate) fn has_admission(&self) -> bool {
        // The governor admits a bot device itself, so its installation is
        // its admission; an agent row is not, which keeps other people's
        // agents out.
        self.admission.is_some()
            || !self.admissions.is_empty()
            || self
                .delegate
                .as_ref()
                .is_some_and(|row| row.installation.is_some())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RedemptionReceipt {
    pub(crate) invite_id: [u8; 32],
    pub(crate) user_public_key: [u8; 32],
    pub(crate) device_public_key: [u8; 32],
    pub(crate) request_id: [u8; 32],
    pub(crate) role: crate::join::MemberRole,
}

fn member_role_from_role(role: Role) -> Option<crate::join::MemberRole> {
    match role {
        Role::Viewer => Some(crate::join::MemberRole::Viewer),
        Role::Commenter => Some(crate::join::MemberRole::Commenter),
        Role::Editor => Some(crate::join::MemberRole::Editor),
        Role::Moderator => Some(crate::join::MemberRole::Moderator),
        Role::Owner => None,
    }
}

/// The map at `key` of a governed room document.
fn governed_map(doc: &AutoCommit, key: &str) -> Result<automerge::ObjId, String> {
    doc.get(ROOT, key)
        .map_err(err)?
        .map(|(_, id)| id)
        .ok_or_else(|| format!("governed {key} missing"))
}

/// Record `user_public_key`'s role in the room's `members` map.
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

/// The room key recording when each member was admitted: person hex →
/// JSON Unix milliseconds.
pub(crate) const MEMBERS_JOINED: &str = "members_joined_v1";

/// Record that the room admitted `user_public_key` at `at_ms` (Unix
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

/// Forget when `person` was admitted, as they leave the room.
pub(crate) fn delete_member_joined(doc: &mut AutoCommit, person: &str) -> Result<(), String> {
    let Some((_, joined)) = doc.get(ROOT, MEMBERS_JOINED).map_err(err)? else {
        return Ok(());
    };
    if doc.get(&joined, person).map_err(err)?.is_some() {
        doc.delete(&joined, person).map_err(err)?;
    }
    Ok(())
}

/// The same native room CRDT keys projected by `WorkspaceTree::sync_room_doc`.
///
/// # Errors
///
/// Returns an error if `room` is not a UUID or the room document cannot be built.
pub fn seed_room(
    room: &str,
    authority: &DeviceCert,
    roster: &[DeviceCert],
) -> Result<Vec<Vec<u8>>, String> {
    uuid::Uuid::parse_str(room).map_err(err)?;
    let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(
        authority.device_public_key.to_vec(),
    ));
    doc.put(ROOT, "room", room).map_err(err)?;
    doc.put(ROOT, "name", "Shared browser workspace")
        .map_err(err)?;
    let owner = Principal::person_from_key(Ed25519PublicKey::from_bytes(authority.user_public_key));
    doc.put(ROOT, "owner", owner.id()).map_err(err)?;
    doc.put_object(ROOT, "members", ObjType::Map).map_err(err)?;
    let grants = doc.put_object(ROOT, "grants", ObjType::Map).map_err(err)?;
    for grant in
        tilo_action_policy::roles::room_role_grants(room, &Subject::Person(owner.id().to_owned()))
    {
        doc.put(
            &grants,
            grant.id.as_str(),
            automerge::ScalarValue::Bytes(serde_json::to_vec(&grant).map_err(err)?),
        )
        .map_err(err)?;
    }
    doc.put_object(ROOT, "invitation_receipts_v1", ObjType::Map)
        .map_err(err)?;
    let names = doc.put_object(ROOT, "peers", ObjType::Map).map_err(err)?;
    let identities = doc
        .put_object(ROOT, "peer_identities_v1", ObjType::Map)
        .map_err(err)?;
    for certificate in roster {
        let id = hex::encode(certificate.device_public_key);
        doc.put(&names, &id, certificate.device_label.as_str())
            .map_err(err)?;
        let identity = GovernedIdentity {
            device_cert: certificate.clone(),
            revocations: vec![],
            admission: None,
            admissions: vec![],
            workspace_delegations: vec![],
            delegate: None,
        };
        doc.put(
            &identities,
            &id,
            automerge::ScalarValue::Bytes(serde_json::to_vec(&identity).map_err(err)?),
        )
        .map_err(err)?;
    }
    doc.commit();
    Ok(doc
        .get_changes(&[])
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect())
}

#[cfg(test)]
mod delivery_admission_resource_tests {
    use super::*;
    #[test]
    fn delivery_admission_local_snapshot_quota_failure_leaves_governance_unchanged() {
        let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
        peer.select_admission_v5().unwrap();
        // Populate the real bounded index near its byte ceiling; no mocked admission/store.
        let full = DocumentAdmission {
            key: crate::admission::AdmissionKey {
                room: String::new(),
                app: String::new(),
                record: "x".repeat(crate::MAX_EXACT_ADMISSION_BYTES - 100),
                author: [0; 32],
                sequence: 0,
            },
            operation_id: [1; 32],
        };
        peer.core.exact_admissions.insert(&full).unwrap();
        let before = peer.durable().unwrap();
        let document = peer.document("test.app", "tab:1").unwrap();
        assert!(
            peer.publish_document("test.app", "tab:1", document, &key, now)
                .is_err()
        );
        assert!(
            peer.durable().unwrap() == before,
            "quota failure changed governance before admission"
        );
    }
    #[test]
    fn delivery_admission_review_quota_rejected_pending_checkpoint_does_not_refresh_route() {
        let (mut receiver, key, _, _, now) = crate::admission_tests::seeded();
        let mut author = receiver.clone();
        let later = now + Duration::from_secs(301);
        let snapshot = author
            .publish_document(
                "test.app",
                "tab:1",
                author.document("test.app", "tab:1").unwrap(),
                &key,
                later,
            )
            .unwrap();
        let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
        receiver.select_admission_v5().unwrap();
        let pending = receiver
            .receive_from_report(origin, snapshot, later)
            .unwrap();
        assert_eq!(
            pending.primary(),
            &InputDisposition::RetryLater(MissingPrerequisite::CheckpointCoverage)
        );
        assert_eq!(receiver.pending_count(), 1);
        assert!(!receiver.fresh_checkpoint_for("test.app", "tab:1", later));
        let checkpoint_times = receiver.core.checkpoint_times.clone();
        let rows = receiver.rows();
        let watermarks = receiver.replay_floors();
        // Fill the actual bounded index so the otherwise valid pending checkpoint
        // fails insertion inside a successful governance-triggered drain.
        receiver
            .core
            .exact_admissions
            .insert(&DocumentAdmission {
                key: crate::admission::AdmissionKey {
                    room: String::new(),
                    app: String::new(),
                    record: "x".repeat(crate::MAX_EXACT_ADMISSION_BYTES - 100),
                    author: [0; 32],
                    sequence: 0,
                },
                operation_id: [1; 32],
            })
            .unwrap();
        let index = receiver.core.exact_admissions.encode().unwrap();
        let report = receiver
            .receive_from_report(origin, author.room_message(), later)
            .unwrap();
        assert_eq!(report.summary().rejected, 1);
        assert!(report.settled().is_empty());
        assert_eq!(receiver.pending_count(), 0);
        assert_eq!(receiver.rows(), rows);
        assert_eq!(receiver.replay_floors(), watermarks);
        assert_eq!(receiver.core.exact_admissions.encode().unwrap(), index);
        assert!(
            !receiver.fresh_checkpoint_for("test.app", "tab:1", later),
            "quota-rejected pending checkpoint refreshed the route"
        );
        assert_eq!(receiver.core.checkpoint_times, checkpoint_times);
    }
    #[test]
    fn delivery_admission_restore_refuses_index_count_bomb_and_unbound_evidence() {
        let literal = include_str!("../tests/fixtures/delivery-admission/native-v5.postcard.hex")
            .split_ascii_whitespace()
            .map(|byte| u8::from_str_radix(byte, 16).unwrap())
            .collect::<Vec<_>>();
        let (peer, _, _, _, now) = crate::admission_tests::seeded();
        let value: DurableV5<'_> = postcard::from_bytes(&literal).unwrap();
        let bomb = postcard::to_allocvec(&DurableV5 {
            version: 5,
            base: value.base,
            exact: &[0x81, 0x80, 0x04],
        })
        .unwrap();
        assert!(
            StatePeer::restore(
                &bomb,
                peer.room(),
                peer.authority().clone(),
                vec![],
                RevocationList::new(),
                now
            )
            .is_err()
        );
        let value: DurableV5<'_> = postcard::from_bytes(&literal).unwrap();
        let mut index = ExactAdmissions::decode(value.exact).unwrap();
        let first = index
            .iter()
            .next()
            .map(|(key, id)| DocumentAdmission {
                key: key.clone(),
                operation_id: *id,
            })
            .unwrap();
        let mut invalid = first;
        invalid.key.sequence += 1;
        index.insert(&invalid).unwrap();
        let exact = index.encode().unwrap();
        let bytes = postcard::to_allocvec(&DurableV5 {
            version: 5,
            base: value.base,
            exact: &exact,
        })
        .unwrap();
        assert!(
            StatePeer::restore(
                &bytes,
                peer.room(),
                peer.authority().clone(),
                vec![],
                RevocationList::new(),
                now
            )
            .is_err()
        );
    }
}

#[cfg(test)]
#[path = "state/media_tests.rs"]
mod room_media_native_tests;

#[cfg(test)]
#[path = "state/bot_members_tests.rs"]
pub(crate) mod bot_members_tests;

#[cfg(test)]
#[path = "state/administrators_tests.rs"]
mod administrators_tests;
#[cfg(test)]
#[path = "state/governance_cache_tests.rs"]
mod governance_cache_tests;
#[cfg(test)]
#[path = "state/joined_tests.rs"]
mod joined_tests;
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
