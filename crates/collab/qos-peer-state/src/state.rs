//! Certified room and document replica. Transport identity is always an input.
use crate::admission::{
    AdmissionError, AdmissionReport, DocumentAdmission, ExactAdmissions, InputDisposition,
    MissingPrerequisite,
};
use crate::cached_document::CachedDocument;
use crate::verified_certificate::VerifiedCertificate;
use crate::{NeighbourMsg, document, room};
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, transaction::Transactable};
use collab_types::caps::{Actor, CapDecision, Principal, ResourceRef, decide_capability};
use collab_types::caps::{CapOverride, Scope, Subject, Verb};
use collab_types::{
    ArtifactKind, ChannelState, DocumentMutation, DocumentWireVersion, Ed25519PublicKey, OpClass,
    Role, SignedOp,
};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use tilo_user_identity::{DeviceCert, RevocationList};
use web_time::{Duration, SystemTime};
mod backend;
mod bounded;
pub use bounded::{
    BoundedPeerError, BoundedPeerInvalid, BoundedStatePeer, PeerDeliveryPolicy,
    PeerDeliveryPolicyError, PeerDocumentRead, PeerDocumentReadError, PeerLimits, PeerMediaCall,
    PeerMember, PeerPolicyBasis, PeerRead, PeerRegistry, PeerRevocations, PeerRoomAccess, PeerTurn,
    PeerWork,
};
#[cfg(test)]
pub(crate) mod json_resource_tests;
use backend::{LegacyNative, PeerCore, PeerDocument};
mod local_admission;
mod local_outbox;
mod owner_membership;
pub use local_outbox::LocalRecoveryReservation;
mod media_governance;
mod native_publication;
pub use native_publication::NativePublicationCut;
mod resources;
pub use media_governance::VerifiedSecurityMetadataOnly;

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
pub fn peer_handle(key: &[u8; 32]) -> u64 {
    key.iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentRow {
    pub app_id: String,
    pub record_id: String,
    pub heads: Vec<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernedAccess {
    pub member_devices: Vec<[u8; 32]>,
    pub member_user_roots: Vec<[u8; 32]>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernedMember {
    pub user_public_key: [u8; 32],
    pub role: crate::join::MemberRole,
    pub display_name: Option<String>,
    pub active_device_keys: Vec<[u8; 32]>,
    pub removed: bool,
}
#[derive(Clone, Debug, Default)]
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
        self.0.bytes == other.0.bytes && self.0.wire_version == other.0.wire_version
    }
}
struct CheckpointBytes {
    bytes: Vec<u8>,
    wire_version: Option<DocumentWireVersion>,
    decoded: std::sync::OnceLock<Result<std::sync::Arc<DecodedCheckpoint>, String>>,
}
struct DecodedCheckpoint {
    doc: automerge::Automerge,
    heads: Vec<String>,
    payload_hash: [u8; 32],
}
impl AcceptedCheckpoint {
    fn new(bytes: Vec<u8>, wire_version: Option<DocumentWireVersion>) -> Self {
        Self(std::sync::Arc::new(CheckpointBytes {
            bytes,
            wire_version,
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
        CHECKPOINT_DECODES.with(|count| count.set(count.get() + 1));
        let doc = automerge::Automerge::load(&self.0.bytes).map_err(err)?;
        let heads = doc.get_heads().iter().map(ToString::to_string).collect();
        let version = self
            .0
            .wire_version
            .ok_or("document wire version unavailable; fresh authenticated checkpoint required")?;
        let payload = DocumentMutation::Snapshot(self.0.bytes.clone())
            .encode_for(version)
            .map_err(err)?;
        Ok(std::sync::Arc::new(DecodedCheckpoint {
            doc,
            heads,
            payload_hash: *blake3::hash(&payload).as_bytes(),
        }))
    }
}

type Document = PeerDocument<LegacyNative>;
#[derive(Serialize, Deserialize)]
struct DurableDocument {
    route: Route,
    bytes: Vec<u8>,
    watermarks: Vec<([u8; 32], u64)>,
    wire_version: Option<DocumentWireVersion>,
    encryption_required: bool,
    covered: BTreeMap<String, u64>,
    covered_snapshot: Option<Vec<u8>>,
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

#[derive(Serialize, Deserialize)]
struct DurableDocumentV2 {
    route: Route,
    bytes: Vec<u8>,
    watermarks: Vec<([u8; 32], u64)>,
    encryption_required: bool,
    covered: BTreeMap<String, u64>,
    covered_snapshot: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize)]
struct DurableV2 {
    version: u32,
    room: String,
    authority: DeviceCert,
    room_bytes: Option<Vec<u8>>,
    documents: Vec<DurableDocumentV2>,
    receipts: BTreeMap<String, Vec<u8>>,
    certificates: Vec<DeviceCert>,
    revocations: Vec<tilo_user_identity::RevocationEntry>,
}
#[derive(Clone)]
pub struct StatePeer {
    core: PeerCore<LegacyNative>,
}

fn governed_snapshot_wire_version(
    room: Option<&AutoCommit>,
    route: &Route,
    snapshot: Option<&[u8]>,
) -> Result<Option<DocumentWireVersion>, String> {
    use crate::coverage::read::{LegacyCoverage, WireVersionError, snapshot_wire_version};
    let room = room.map(crate::room::read::LegacyRead);
    snapshot_wire_version(room.as_ref(), &route.app, &route.record, snapshot, &LegacyCoverage)
        .map_err(|error| match error {
            WireVersionError::Decode(error) => format!("invalid checkpoint coverage: {error}"),
            WireVersionError::Resource(error) => format!("checkpoint coverage resource refusal: {error:?}"),
            WireVersionError::NoMatch => format!(
                "governed checkpoint for {}/{} does not match a supported document wire version; preserve journal and request a fresh authenticated checkpoint",
                route.app, route.record
            ),
            WireVersionError::Ambiguous => format!(
                "governed checkpoint for {}/{} ambiguously matches multiple document wire versions",
                route.app, route.record
            ),
        })
}

impl StatePeer {
    pub fn new(
        room: &str,
        authority: DeviceCert,
        roster: Vec<DeviceCert>,
        revocations: RevocationList,
        now: SystemTime,
    ) -> Result<Self, String> {
        let room = uuid::Uuid::parse_str(room).map_err(err)?.to_string();
        let mut certificates = BTreeMap::new();
        for certificate in roster.into_iter().chain([authority.clone()]) {
            let certificate = VerifiedCertificate::new(certificate, now_ns(now))?;
            if revocations.is_revoked(&certificate.device_public_key) {
                return Err("device revoked".into());
            }
            certificates.insert(certificate.device_public_key, certificate);
        }
        Ok(Self {
            core: PeerCore {
                room,
                authority,
                certificates,
                revocations,
                receipts: BTreeMap::new(),
                room_doc: None,
                documents: BTreeMap::new(),
                pending: VecDeque::new(),
                pending_bytes: 0,
                checkpoint_times: BTreeMap::new(),
                exact_admissions: ExactAdmissions::default(),
                admission_v5: false,
                native_cut_publisher: None,
                retention: (),
            },
        })
    }
    /// Add a root-certified viewer to existing governed history. Only the pinned
    /// room authority may issue this local governance mutation.
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
        if let Some(old) = self.core.certificates.get(&cert.device_public_key) {
            if old.as_ref() != &cert {
                return Err("certificate conflict".into());
            }
        }
        let mut doc = self
            .core
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let identity = GovernedIdentity {
            device_cert: cert.clone(),
            revocations: vec![],
            admission: None,
            admissions: vec![],
            workspace_delegations: vec![],
        };
        let id = hex::encode(cert.device_public_key);
        let encoded = serde_json::to_vec(&identity).map_err(err)?;
        if room::governance_of(Some(&doc))
            .3
            .get(&id)
            .and_then(|bytes| serde_json::from_slice::<GovernedIdentity>(bytes).ok())
            .is_some_and(|old| old.device_cert == cert)
        {
            return Ok(false);
        }
        use automerge::ReadDoc;
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
        doc.put(&names, &id, cert.device_label.as_str())
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
    /// Publish same-root, root-signed revocations into pinned governance.
    /// Independent collaborator roots retain their own signed revocation scope.
    pub fn publish_governed_revocations(
        &mut self,
        entries: &[tilo_user_identity::RevocationEntry],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        self.require_governance_writer(key, now)?;
        let mut verified = self.core.revocations.clone();
        for entry in entries {
            if entry.user_public_key != self.core.authority.user_public_key {
                return Err("governed revocation root mismatch".into());
            }
            verified.insert(entry.clone()).map_err(err)?;
        }
        let mut doc = self
            .core
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let before = doc.get_heads();
        let identities = doc
            .get(ROOT, "peer_identities_v1")
            .map_err(err)?
            .ok_or("governed identities missing")?
            .1;
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        let mut changed = false;
        for (id, bytes) in room::governance_of(Some(&doc)).3 {
            let mut identity: GovernedIdentity = serde_json::from_slice(&bytes).map_err(err)?;
            if identity.device_cert.user_public_key != self.core.authority.user_public_key {
                continue;
            }
            let mut list = RevocationList::new();
            for entry in &identity.revocations {
                list.insert(entry.clone()).map_err(err)?;
            }
            for entry in verified.iter() {
                if entry.revoked_device_public_key == identity.device_cert.device_public_key
                    || identity.device_cert == self.core.authority
                {
                    list.insert(entry.clone()).map_err(err)?;
                }
            }
            let mut projected = list.iter().cloned().collect::<Vec<_>>();
            projected.sort_by_key(|entry| entry.revoked_device_public_key);
            identity.revocations = projected;
            let encoded = serde_json::to_vec(&identity).map_err(err)?;
            if encoded != bytes {
                doc.put(&identities, id, automerge::ScalarValue::Bytes(encoded))
                    .map_err(err)?;
                changed = true;
            }
        }
        if changed {
            doc.commit();
            self.apply_governance_doc(doc, &before, key, now)?;
        } else {
            self.core.revocations = verified;
        }
        Ok(())
    }
    pub fn install_workspace_delegation(
        &mut self,
        delegation: crate::join::WorkspaceDelegation,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.require_governance_writer(key, now)?;
        if self
            .core
            .certificates
            .get(&delegation.delegate.device_public_key)
            .map(AsRef::as_ref)
            != Some(&delegation.delegate)
        {
            return Err("delegated workspace service is absent from governed roster".into());
        }
        let cached = self
            .core
            .room_doc
            .as_ref()
            .ok_or("room governance missing")?;
        let mut document = (**cached).clone();
        match crate::governance::install_workspace_delegation(
            &mut document,
            &self.core.authority,
            delegation,
            key,
            now,
        )? {
            crate::governance::GovernanceOutcome::Unchanged => Ok(false),
            crate::governance::GovernanceOutcome::Applied { changes } => {
                let room = self.core.room.clone();
                let outcome = self.receive_from(
                    // The shared governance transaction has already verified
                    // either the owner or its persisted room delegation.
                    TransportOrigin::RelayDelta(self.core.authority.device_public_key),
                    NeighbourMsg::SyncResp { changes, room },
                    now,
                )?;
                Ok(outcome.changed || outcome.metadata_changed)
            }
        }
    }
    /// Atomically records a verified single-use invitation redemption, its
    /// independent root-certified device, and its room role.
    pub fn admit_invited_member(
        &mut self,
        redemption: &crate::join::VerifiedRedemption,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let grant = redemption.join().grant();
        if grant.room != self.core.room || grant.authority != self.core.authority {
            return Err("invitation is not for this pinned room authority".into());
        }
        if grant.issuer == self.core.authority {
            self.require_governance_writer(key, now)?;
        } else {
            if key.verifying_key().to_bytes() != grant.issuer.device_public_key {
                return Err("governance writer is not invitation issuer".into());
            }
            if self.certificate(&grant.issuer.device_public_key, now)? != &grant.issuer {
                return Err("delegated invitation issuer certificate mismatch".into());
            }
        }
        let cert = redemption.certificate();
        if self.core.revocations.is_revoked(&cert.device_public_key) {
            return Err("device revoked".into());
        }
        if let Some(old) = self.core.certificates.get(&cert.device_public_key)
            && old.as_ref() != cert
        {
            return Err("certificate conflict".into());
        }
        let cached = self
            .core
            .room_doc
            .as_ref()
            .ok_or("room governance missing")?;
        let mut document = (**cached).clone();
        match crate::governance::admit_invited_member(&mut document, redemption, key, now)? {
            crate::governance::AdmissionOutcome::AlreadyApplied => Ok(false),
            crate::governance::AdmissionOutcome::Applied { changes } => {
                let room = self.core.room.clone();
                let outcome = self.receive_from(
                    TransportOrigin::RelayDelta(self.core.authority.device_public_key),
                    NeighbourMsg::SyncResp { changes, room },
                    now,
                )?;
                Ok(outcome.changed || outcome.metadata_changed)
            }
        }
    }

    pub fn set_member_role(
        &mut self,
        user_public_key: [u8; 32],
        role: crate::join::MemberRole,
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
        let before_role = room::role_of(
            policy.owner.as_deref(),
            &policy.caps,
            &hex::encode(user_public_key),
            now_secs(now),
        );
        let before = doc.get_heads();
        let caps = doc
            .get(ROOT, "capabilities")
            .map_err(err)?
            .ok_or("governed capabilities missing")?
            .1;
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        put_member_capability(&mut doc, &caps, user_public_key, role)?;
        doc.commit();
        let changed = self.apply_governance_doc(doc, &before, key, now)?;
        Ok(changed && before_role != Some(member_role(role)))
    }

    pub fn remove_member(
        &mut self,
        user_public_key: [u8; 32],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.require_governance_writer(key, now)?;
        if user_public_key == self.core.authority.user_public_key {
            return Err("workspace owner cannot be removed".into());
        }
        let mut doc = self
            .core
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let (_, capabilities) = doc
            .get(ROOT, "capabilities")
            .map_err(err)?
            .ok_or("governed capabilities missing")?;
        let person = hex::encode(user_public_key);
        let matching = doc
            .keys(&capabilities)
            .filter(|id| {
                doc.get(&capabilities, id)
                    .ok()
                    .flatten()
                    .is_some_and(|(value, _)| {
                        let automerge::Value::Scalar(value) = value else {
                            return false;
                        };
                        let automerge::ScalarValue::Bytes(bytes) = value.as_ref() else {
                            return false;
                        };
                        serde_json::from_slice::<CapOverride>(bytes).is_ok_and(|cap| {
                            subject_is_person(&cap.subject, user_public_key, &person)
                                && matches!(cap.scope, Scope::Room(_) | Scope::Workspace)
                        })
                    })
            })
            .collect::<Vec<_>>();
        if matching.is_empty() {
            return Ok(false);
        }
        let before = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        for id in matching {
            doc.delete(&capabilities, id).map_err(err)?;
        }
        doc.commit();
        self.apply_governance_doc(doc, &before, key, now)
    }

    pub fn governed_access(&self, now: SystemTime) -> Result<GovernedAccess, String> {
        let policy = self
            .core
            .room_doc
            .as_ref()
            .ok_or("room governance missing")?
            .policy();
        let mut member_devices = Vec::new();
        let mut member_user_roots = Vec::new();
        for bytes in policy.identities.values() {
            let identity: GovernedIdentity = serde_json::from_slice(bytes).map_err(err)?;
            let cert = identity.device_cert;
            if tilo_user_identity::verify_device_cert_at(&cert, now_ns(now)).is_err()
                || self.core.revocations.is_revoked(&cert.device_public_key)
            {
                continue;
            }
            let person = hex::encode(cert.user_public_key);
            if room::role_of(
                policy.owner.as_deref(),
                &policy.caps,
                &person,
                now_secs(now),
            )
            .is_some_and(|role| role != Role::Denied)
            {
                member_devices.push(cert.device_public_key);
                member_user_roots.push(cert.user_public_key);
            }
        }
        member_devices.sort_unstable();
        member_devices.dedup();
        member_user_roots.sort_unstable();
        member_user_roots.dedup();
        Ok(GovernedAccess {
            member_devices,
            member_user_roots,
        })
    }

    /// Authority-authored collaborator projection. The structural owner is
    /// exposed separately by [`StatePeer::owner_user_public_key`].
    pub fn governed_members(&self, now: SystemTime) -> Result<Vec<GovernedMember>, String> {
        let doc = self
            .core
            .room_doc
            .as_ref()
            .ok_or("room governance missing")?;
        let policy = doc.policy();
        let names = room::governance_of(Some(doc)).2;
        let mut rows: BTreeMap<[u8; 32], GovernedMember> = BTreeMap::new();
        for (id, bytes) in &policy.identities {
            let identity: GovernedIdentity = serde_json::from_slice(bytes).map_err(err)?;
            let Some(receipt) = identity
                .admission
                .as_ref()
                .or_else(|| identity.admissions.last())
            else {
                continue;
            };
            let root = identity.device_cert.user_public_key;
            let person = hex::encode(root);
            let active_role = room::role_of(
                policy.owner.as_deref(),
                &policy.caps,
                &person,
                now_secs(now),
            )
            .and_then(member_role_from_role);
            let row = rows.entry(root).or_insert_with(|| GovernedMember {
                user_public_key: root,
                role: active_role.unwrap_or(receipt.role),
                display_name: names.get(id).cloned(),
                active_device_keys: Vec::new(),
                removed: active_role.is_none(),
            });
            if let Some(role) = active_role {
                row.role = role;
                row.removed = false;
                let cert = &identity.device_cert;
                if tilo_user_identity::verify_device_cert_at(cert, now_ns(now)).is_ok()
                    && !self.core.revocations.is_revoked(&cert.device_public_key)
                {
                    row.active_device_keys.push(cert.device_public_key);
                }
            }
            if row.display_name.is_none() {
                row.display_name = names.get(id).cloned();
            }
        }
        let mut rows = rows.into_values().collect::<Vec<_>>();
        for row in &mut rows {
            row.active_device_keys.sort_unstable();
            row.active_device_keys.dedup();
        }
        Ok(rows)
    }

    pub fn owner_user_public_key(&self) -> [u8; 32] {
        self.core.authority.user_public_key
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Keep the membership challenge's authenticated protocol fields explicit"
    )]
    pub fn membership_challenge(
        &self,
        workspace_id: u64,
        device_cert: &DeviceCert,
        request_id: [u8; 32],
        nonce: [u8; 32],
        expires_at_ns: u64,
        now: SystemTime,
    ) -> Result<crate::join::MembershipChallenge, String> {
        if self
            .core
            .certificates
            .get(&device_cert.device_public_key)
            .map(AsRef::as_ref)
            != Some(device_cert)
        {
            return Err("membership certificate is absent from governed state".into());
        }
        self.certificate(&device_cert.device_public_key, now)?;
        let role = match self.permissions(device_cert.device_public_key, now) {
            Ok((role, _, _)) => role,
            Err(error)
                if error.contains("no room membership")
                    && self.has_governed_admission(device_cert) =>
            {
                return Err("member has been removed".into());
            }
            Err(error) => return Err(error),
        };
        if matches!(role, Role::Denied) {
            return Err("member has been removed".into());
        }
        crate::join::MembershipChallenge::new(
            &self.core.room,
            workspace_id,
            &self.core.authority,
            device_cert,
            request_id,
            nonce,
            expires_at_ns,
            now_ns(now),
        )
    }

    pub fn verify_member_refresh(
        &self,
        membership: &crate::join::VerifiedMembership,
        now: SystemTime,
    ) -> Result<crate::join::MemberRole, String> {
        let role = self.current_membership_role(membership, now)?;
        member_role_from_role(role)
            .ok_or("structural owner does not use collaborator refresh".into())
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
        self.core.certificates = certificates;
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
    pub fn permissions(
        &self,
        source: [u8; 32],
        now: SystemTime,
    ) -> Result<(Role, Vec<collab_types::caps::CapOverride>, String), String> {
        let role = self.role(&source, now)?;
        let cert = self.certificate(&source, now)?;
        let caps = self
            .core
            .room_doc
            .as_ref()
            .ok_or("room missing")?
            .policy()
            .caps
            .clone();
        let person = Principal::person_from_key(Ed25519PublicKey::from_bytes(cert.user_public_key));
        Ok((role, caps, person.id().to_owned()))
    }
    /// Committed, admitted room only. Callers cannot mutate the admission owner.
    pub fn admitted_room(&self) -> Option<&AutoCommit> {
        self.core.room_doc.as_deref()
    }

    /// Read permission for an exact canonical record using the same governed
    /// actor/resource vocabulary as document mutation admission.
    pub fn check_document_read(
        &self,
        device: [u8; 32],
        app: &str,
        record: &str,
        now: SystemTime,
    ) -> Result<(), String> {
        let (role, caps, person) = self.permissions(device, now)?;
        if !backend::access::document_read(role, &caps, &person, &self.core.room, app, record, now)
        {
            return Err(format!("record read forbidden: {app}/{record}"));
        }
        Ok(())
    }
    pub fn has_governance(&self) -> bool {
        self.core.room_doc.is_some()
    }
    pub fn incomplete_reason(&self) -> Option<String> {
        self.core.documents.values().any(|entry|entry.encryption_required).then(||"incomplete: authenticated persisted MLS group required; plaintext downgrade forbidden".into())
    }
    /// Validated data from this exact snapshot, not an authorization credential.
    /// Missing exact Chat prerequisites and unpromoted local revocations fail
    /// closed. Current device permissions/time still require separate checks.
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
    /// acknowledgement: covers() additionally requires causal change inclusion.
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
        let Ok(collab_types::VersionedRoutedDocumentPayload::Cleartext(decoded)) =
            collab_types::validate_document_payload_route_versioned(
                app_id, cluster_id, record_id, envelope,
            )
        else {
            return false;
        };
        if document.wire_version != Some(decoded.version) {
            return false;
        }
        let DocumentMutation::Incremental(bytes) = decoded.mutation else {
            return false;
        };
        let Ok(checkpoint) = snapshot.decoded() else {
            return false;
        };
        let mut proof = checkpoint.doc.clone();
        let before = proof.get_heads();
        // A sequence floor is not causal coverage: an out-of-order incremental
        // may have been validated while its dependencies were unavailable.
        proof.load_incremental(&bytes).is_ok()
            && proof.get_missing_deps(&[]).is_empty()
            && proof.get_heads() == before
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

    pub(crate) fn document_wire_version(
        &self,
        app: &str,
        record: &str,
    ) -> Option<DocumentWireVersion> {
        self.core
            .documents
            .get(&Route {
                app: app.into(),
                record: record.into(),
            })
            .and_then(|document| document.wire_version)
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
    pub fn document(&self, app: &str, record: &str) -> Option<Vec<u8>> {
        self.core
            .documents
            .get(&Route {
                app: app.into(),
                record: record.into(),
            })
            .map(|entry| entry.doc.clone().save())
    }
    fn certificate(&self, source: &[u8; 32], now: SystemTime) -> Result<&DeviceCert, String> {
        self.core.certificate_typed(source, now).map_err(err)
    }
    fn role(&self, source: &[u8; 32], now: SystemTime) -> Result<Role, String> {
        backend::member::legacy(&self.core, source, now).map_err(err)
    }
    fn authorize_document(
        &self,
        source: &[u8; 32],
        app_id: &str,
        envelope: &SignedOp,
        authorization_time: SystemTime,
    ) -> Result<Role, String> {
        let role = self.role(source, authorization_time)?;
        let cert = self.certificate(source, authorization_time)?;
        let (_, _, caps) = room::extract_governed_fields(
            self.core.room_doc.as_deref().ok_or("missing governance")?,
        );
        let person = Principal::person_from_key(Ed25519PublicKey::from_bytes(cert.user_public_key));
        let actor = Actor::person(Some(role), person.id())
            .with_principal(Principal::App(app_id.to_owned()))
            .in_room(self.core.room.clone());
        let resource = ResourceRef::Artifact {
            app: Some(app_id.to_owned()),
            kind: ArtifactKind::Note,
            instance: envelope.artifact,
        };
        if decide_capability(
            &actor,
            &resource,
            collab_types::caps::verb_for_opclass(envelope.op_class),
            &caps,
            Some(now_secs(authorization_time)),
        ) != CapDecision::Allow
        {
            return Err("document capability forbidden".into());
        }
        Ok(role)
    }

    /// Select native schema v5 only as part of an outer owner's reviewed rollout.
    /// This selects candidate serialization; it does not commit any bytes.
    pub fn select_admission_v5(&mut self) -> Result<(), AdmissionError> {
        self.core.exact_admissions.encode()?;
        self.core.admission_v5 = true;
        Ok(())
    }
    pub fn admission_v5_selected(&self) -> bool {
        self.core.admission_v5
    }

    /// Exact observations from the real admission path, not a durability receipt.
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
        let (source, retained_replay) = self.resolve_origin(origin, &message, now)?;
        let mut candidate = self.clone();
        let report = candidate.receive_with_context(source, message, now, retained_replay, true)?;
        *self = candidate;
        Ok(report)
    }

    /// A read-only exact retry lookup. Current authority and signature/route still
    /// apply; only a recorded envelope bypasses new-write timestamp freshness.
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
        self.authorize_document(&source, app, op, now)?;
        collab_types::validate_document_payload_route_versioned(app, room, record, op)
            .map_err(err)?;
        collab_types::verify_op(op).map_err(err)?;
        let admission = DocumentAdmission::from_op(room, app, record, op)?;
        match self.core.exact_admissions.get(&admission.key) {
            Some(id) if id == &admission.operation_id => Ok(Some(admission)),
            Some(_) => Err(AdmissionError::ConflictingSequence),
            None => Ok(None),
        }
    }

    /// Exact new candidate admissions for local multi-record owner commits.
    /// Also refuses replacement that discards prior exact evidence.
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
        for (key, id) in before.core.exact_admissions.iter() {
            if self.core.exact_admissions.get(key) != Some(id) {
                return Err("admission candidate discards exact evidence".into());
            }
        }
        Ok(self
            .core
            .exact_admissions
            .iter()
            .filter(|(key, _)| before.core.exact_admissions.get(key).is_none())
            .map(|(key, id)| DocumentAdmission {
                key: key.clone(),
                operation_id: *id,
            })
            .collect())
    }

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
            NeighbourMsg::IdentityHello { cert, revocations } => {
                if cert.device_public_key != source {
                    return Err("transport certificate mismatch".into());
                }
                if self.core.certificates.get(&source).map(AsRef::as_ref) != Some(cert.as_ref()) {
                    let admitted = self
                        .core
                        .room_doc
                        .as_deref()
                        .and_then(|doc| {
                            room::governance_of(Some(doc))
                                .3
                                .get(&hex::encode(source))
                                .cloned()
                        })
                        .and_then(|bytes| serde_json::from_slice::<GovernedIdentity>(&bytes).ok());
                    if admitted
                        .as_ref()
                        .is_none_or(|identity| identity.device_cert != *cert)
                    {
                        return Err("identity is absent from admitted roster".into());
                    }
                }
                let verified = VerifiedCertificate::new((*cert).clone(), now_ns(now))?;
                let mut metadata_changed =
                    self.core.certificates.get(&source).map(AsRef::as_ref) != Some(cert.as_ref());
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
            NeighbourMsg::SyncResp {
                changes,
                room: received,
            } => {
                if received != self.core.room {
                    return Err("room topic mismatch".into());
                }
                self.certificate(&source, now)?;
                if self.core.room_doc.is_some() || source != self.core.authority.device_public_key {
                    let role = self.role(&source, now)?;
                    let certificate = self.certificate(&source, now)?;
                    let person = Principal::person_from_key(Ed25519PublicKey::from_bytes(
                        certificate.user_public_key,
                    ));
                    let (_, _, caps) = room::extract_governed_fields(
                        self.core
                            .room_doc
                            .as_deref()
                            .ok_or("room governance missing")?,
                    );
                    let actor =
                        Actor::person(Some(role), person.id()).in_room(self.core.room.clone());
                    if decide_capability(
                        &actor,
                        &ResourceRef::Room(self.core.room.clone()),
                        collab_types::caps::Verb::Write,
                        &caps,
                        Some(now_secs(now)),
                    ) != CapDecision::Allow
                    {
                        return Err("room write forbidden".into());
                    }
                }
                let candidate =
                    room::candidate(self.core.room_doc.as_deref(), &changes).map_err(err)?;
                if room::declared_room_mismatches(
                    &candidate,
                    uuid::Uuid::parse_str(&self.core.room).map_err(err)?,
                ) {
                    return Err("declared room mismatch".into());
                }
                let pinned = hex::encode(self.core.authority.device_public_key);
                let from = hex::encode(source);
                if !room::governance_change_allowed(
                    self.core.room_doc.as_deref(),
                    &candidate,
                    false,
                    Some(&pinned),
                    &from,
                ) {
                    return Err("governance writer is not pinned authority".into());
                }
                crate::topology::validate(
                    self.core.room_doc.as_deref(),
                    &candidate,
                    &from,
                    &pinned,
                )?;
                if !room::immutable_receipts(self.core.room_doc.as_deref(), &candidate) {
                    return Err("effect receipt is immutable".into());
                }
                let mut receipts = self.core.receipts.clone();
                for (key, value) in room::receipt_values(&candidate) {
                    let bytes = serde_json::to_vec(&value).map_err(err)?;
                    if receipts.get(&key).is_some_and(|prior| prior != &bytes) {
                        return Err("effect receipt is immutable".into());
                    }
                    receipts.insert(key, bytes);
                }
                crate::coverage::monotonic(self.core.room_doc.as_deref(), &candidate)?;
                crate::durability::monotonic(self.core.room_doc.as_deref(), &candidate)?;
                let changed = self
                    .core
                    .room_doc
                    .as_deref()
                    .is_none_or(|doc| doc.base_heads() != candidate.base_heads());
                let media = room::media::validate_transition(
                    self.core.room_doc.as_deref(),
                    &candidate,
                    room::media::TransitionMode::Admission,
                )
                .map_err(err)?;
                if let Some(policy) = &media {
                    self.validate_media_bindings(policy, false)?;
                }
                let mut metadata_changed = self.import_governed_certificates(&candidate, now)?;
                self.core.receipts = receipts;
                self.core.room_doc = Some(candidate.into());
                let pending = std::mem::take(&mut self.core.pending);
                self.core.pending_bytes = 0;
                let mut applied = changed;
                let mut rejected = 0;
                let mut settled = Vec::new();
                for (author, message, replayed) in pending {
                    match self.receive_with_context(author, message, now, replayed, strict_report) {
                        Ok(outcome) => {
                            applied |= outcome.changed;
                            metadata_changed |= outcome.metadata_changed;
                            rejected += outcome.rejected;
                            settled.extend(outcome.settled);
                        }
                        Err(error) => {
                            if self.core.native_cut_publisher.is_some() {
                                return Err(error);
                            }
                            rejected += 1;
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
        if self.core.pending.len() >= 64
            || bytes.len() > 4 * 1024 * 1024usize - self.core.pending_bytes
        {
            return Err("app prerequisite queue full; fresh checkpoint required".into());
        }
        self.core.pending_bytes += bytes.len();
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
            self.admit_document(source, message, now, false, retained_replay, strict_report)?;
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
                    outcome.rejected += applied.rejected;
                    outcome.settled.extend(applied.settled);
                }
                Err(error) => {
                    if self.core.native_cut_publisher.is_some() {
                        return Err(error);
                    }
                    outcome.rejected += 1;
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
    pub fn admit_local_app_document(
        &mut self,
        app: &str,
        record: &str,
        envelope: collab_types::SignedOp,
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
            true,
            false,
            false,
        )
        .map(|report| report.summary)
        .map_err(err)
    }

    fn admit_document(
        &mut self,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
        local_app: bool,
        retained_replay: bool,
        strict_report: bool,
    ) -> Result<AdmissionReport, AdmissionError> {
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
        if strict_report {
            if let Some(admitted) = self.lookup_exact_document_parts(
                source,
                &app_id,
                &cluster_id,
                &record_id,
                &envelope,
                now,
            )? {
                return Ok(AdmissionReport::new(
                    InputDisposition::Duplicate(admitted),
                    ReceiveOutcome {
                        duplicate: true,
                        ..Default::default()
                    },
                ));
            }
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
        let role = self.authorize_document(&source, &app_id, &envelope, authorization_time)?;
        let payload = collab_types::validate_document_payload_route_versioned(
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
        let mut candidate = self
            .core
            .documents
            .get(&route)
            .cloned()
            .unwrap_or_else(|| Document {
                doc: AutoCommit::new(),
                channel: ChannelState::default(),
                wire_version: None,
                encryption_required: false,
                covered: BTreeMap::new(),
                covered_snapshot: None,
            });
        candidate
            .channel
            .artifacts
            .insert(envelope.artifact, ArtifactKind::Note);
        candidate
            .channel
            .artifact_apps
            .insert(envelope.artifact, app_id.clone());
        let mut validated_channel = candidate.channel.clone();
        match collab_types::validate_admitted_inbound_op(
            &envelope,
            role,
            &mut validated_channel,
            authorization_time,
            if historical_validation {
                Duration::ZERO
            } else {
                Duration::from_secs(300)
            },
        ) {
            Ok(()) => {}
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
        let (mutation, incoming_wire_version)=match payload {
            collab_types::VersionedRoutedDocumentPayload::Cleartext(decoded) if !candidate.encryption_required=>{
                if candidate.wire_version.is_some_and(|existing| existing != decoded.version) {
                    return Err("document route cannot change its authenticated wire version".into());
                }
                (decoded.mutation, decoded.version)
            },
            collab_types::VersionedRoutedDocumentPayload::Encrypted{..}=>{
                // Validate the signed author first, then retain only the missing
                // prerequisite. Ciphertext has not been applied, so its sequence
                // is deliberately not advanced until a real MLS group exists.
                candidate.encryption_required=true;
                self.core.documents.insert(route,candidate);
                return Ok(AdmissionReport::new(InputDisposition::RetryLater(MissingPrerequisite::PersistedMls), ReceiveOutcome{metadata_changed:true,incomplete:self.incomplete_reason(),..Default::default()}));
            },
            _=>return Err("incomplete: authenticated persisted MLS group required; plaintext downgrade forbidden".into()),
        };
        candidate.channel = validated_channel;
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
            let mut snapshot = AutoCommit::new();
            match snapshot
                .apply_incoming_checked(
                    bytes,
                    &crate::document::incoming_document_limits(),
                    &automerge::resource_limits::NativeAllowance::new(64 * 1024 * 1024)
                        .map_err(err)?,
                    automerge::resource_limits::NativeDependencyPolicy::RequireComplete,
                )
                .map_err(err)?
            {
                automerge::resource_limits::NativeIncomingOutcome::Applied { .. } => {}
                automerge::resource_limits::NativeIncomingOutcome::MissingDependencies => {
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
            candidate.wire_version.get_or_insert(incoming_wire_version);
            candidate.covered_snapshot = Some(AcceptedCheckpoint::new(
                bytes.clone(),
                candidate.wire_version,
            ));
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
        } else if candidate.wire_version.is_none() {
            return Err(
                "document wire version unavailable; fresh authenticated checkpoint required".into(),
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
        if let Some(room) = self.core.room_doc.as_deref() {
            if let Some(policy) = room::media::parse(room).map_err(err)? {
                room::media::validate_bound_chat_change(
                    &policy,
                    &route.app,
                    &route.record,
                    &candidate.doc,
                )
                .map_err(err)?;
            }
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
                unreachable!()
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
        let wire_version = match self.core.documents.get(&route) {
            Some(document) => document.wire_version.ok_or(
                "document wire version unavailable; fresh authenticated checkpoint required",
            )?,
            None => DocumentWireVersion::CURRENT,
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
            mutation.encode_for(wire_version).map_err(err)?,
            collab_types::document_artifact_id_for_version(
                wire_version,
                app,
                &self.core.room,
                record,
            ),
            author,
            collab_types::document_attestation_id_for_version(
                wire_version,
                app,
                &self.core.room,
                record,
            ),
            OpClass::Splice,
            now,
            seq,
        );
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
                unreachable!()
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
    fn signed_snapshots_in_place(
        &mut self,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        self.validate_present_media_policy()?;
        let documents = self
            .core
            .documents
            .iter()
            .map(|(route, entry)| (route.clone(), entry.doc.clone().save()))
            .collect::<Vec<_>>();
        let prepared = documents
            .into_iter()
            .map(|(route, bytes)| {
                self.prepare_signed_mutation(
                    &route.app,
                    &route.record,
                    DocumentMutation::Snapshot(bytes),
                    key,
                    now,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (messages, coverages): (Vec<_>, Vec<_>) = prepared.into_iter().unzip();
        let coverages = coverages.into_iter().flatten().collect::<Vec<_>>();
        if !coverages.is_empty() {
            crate::coverage::put_all(
                self.core
                    .room_doc
                    .as_deref_mut()
                    .ok_or("missing room governance")?,
                coverages,
            )?;
        }
        for message in &messages {
            self.receive(key.verifying_key().to_bytes(), message.clone(), now)?;
        }
        Ok(messages)
    }
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
    pub fn bootstrap_messages(
        &mut self,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let mut candidate = self.clone();
        let snapshots = candidate.signed_snapshots_in_place(key, now)?;
        let mut messages = vec![candidate.room_message()];
        messages.extend(snapshots);
        *self = candidate;
        Ok(messages)
    }
    /// Prepare, but do not publish, a candidate bound to the next native commit.
    /// The caller must commit this entire state and outgoing batch before exposing it.
    pub fn durable_bootstrap_messages(
        &mut self,
        key: &SigningKey,
        now: SystemTime,
        generation: u64,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let mut candidate = self.clone();
        let snapshots = candidate.signed_snapshots_in_place(key, now)?;
        let room = candidate
            .core
            .room_doc
            .as_deref_mut()
            .ok_or("missing room governance")?;
        let receipts = crate::coverage::entries(room)?
            .into_iter()
            .map(|(route, coverage)| {
                Ok((
                    route,
                    crate::durability::DurableReceipt::new(coverage, generation)?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        crate::durability::put(room, receipts)?;
        crate::durability::monotonic(self.core.room_doc.as_deref(), room)?;
        let mut messages = vec![candidate.room_message()];
        messages.extend(snapshots);
        *self = candidate;
        Ok(messages)
    }
    /// Prepare route-selective publication against the last committed peer.
    /// SyncResp here contains only room changes since `before` and MUST be retained
    /// as a delta. Application messages remain exact covered snapshots. The caller
    /// must durably commit the candidate and its baseline plus this tail before send.
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
                .get_mut(route)
                .ok_or("publication discards a record")?;
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
        let mut receipts = crate::durability::entries(room)?;
        let mut dirty = Vec::new();
        for (route, document) in &candidate.core.documents {
            let route_key = crate::coverage::route_key(&route.app, &route.record);
            let changed = before.core.documents.get(route).is_none_or(|prior| {
                prior.doc.base_heads() != document.doc.base_heads()
                    || prior.channel.last_seq_per_author != document.channel.last_seq_per_author
                    || prior.wire_version != document.wire_version
                    || prior.encryption_required != document.encryption_required
                    || prior.covered != document.covered
                    || prior.covered_snapshot != document.covered_snapshot
                    || old_coverage.get(&route_key) != coverage.get(&route_key)
                    || old_receipts.get(&route_key) != receipts.get(&route_key)
            });
            if changed {
                dirty.push((route.clone(), document.doc.clone().save()));
            }
        }
        let mut snapshots = Vec::new();
        for (route, bytes) in &dirty {
            snapshots.push(candidate.publish_document(
                &route.app,
                &route.record,
                bytes.clone(),
                key,
                now,
            )?);
        }
        let room = candidate
            .core
            .room_doc
            .as_mut()
            .ok_or("missing room governance")?;
        let coverage = crate::coverage::entries(room)?;
        for (route, _) in &dirty {
            let route_key = crate::coverage::route_key(&route.app, &route.record);
            let covered = coverage
                .get(&route_key)
                .ok_or("missing published coverage")?;
            receipts.insert(
                route_key,
                crate::durability::DurableReceipt::new(covered.clone(), generation)?,
            );
        }
        if !dirty.is_empty() {
            crate::durability::put(room, receipts)?;
        }
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
        *self = candidate;
        Ok(messages)
    }
    /// A governed assertion alone is insufficient: bind it to the accepted exact checkpoint.
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
        let receipts = crate::durability::entries(room)?;
        let Some(receipt) = receipts.get(&crate::coverage::route_key(app, record)) else {
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
        Ok(Some((receipt.clone(), accepted)))
    }
    /// Validate the receipt and current-head coverage against one decoded checkpoint.
    /// All authority, route, payload and watermark checks remain fresh per query.
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
    pub fn durable_covers_current(&self, app: &str, record: &str) -> Result<bool, String> {
        Ok(self
            .durable_coverage_with_current(app, record)?
            .is_some_and(|(_, current)| current))
    }
    pub fn durable(&self) -> Result<Vec<u8>, String> {
        let documents = self
            .core
            .documents
            .iter()
            .map(|(route, entry)| DurableDocument {
                route: route.clone(),
                bytes: entry.doc.clone().save(),
                watermarks: entry
                    .channel
                    .last_seq_per_author
                    .iter()
                    .map(|(key, seq)| (*key.as_bytes(), *seq))
                    .collect(),
                wire_version: entry.wire_version,
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
                receipts: self.core.receipts.clone(),
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
        if self.core.admission_v5 {
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

    #[cfg(test)]
    pub(crate) fn durable_v2_for_test(&self) -> Result<Vec<u8>, String> {
        let documents = self
            .core
            .documents
            .iter()
            .map(|(route, entry)| DurableDocumentV2 {
                route: route.clone(),
                bytes: entry.doc.clone().save(),
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
        postcard::to_allocvec(&DurableV2 {
            version: 2,
            room: self.core.room.clone(),
            authority: self.core.authority.clone(),
            room_bytes: self.core.room_doc.as_ref().map(CachedDocument::saved),
            documents,
            receipts: self.core.receipts.clone(),
            certificates: self
                .core
                .certificates
                .values()
                .map(|certificate| certificate.as_ref().clone())
                .collect(),
            revocations: {
                let mut entries = self.core.revocations.iter().cloned().collect::<Vec<_>>();
                entries.sort_by_key(|entry| entry.revoked_device_public_key);
                entries
            },
        })
        .map_err(err)
    }

    #[cfg(test)]
    pub(crate) fn durable_v2_without_covered_snapshot_for_test(&self) -> Result<Vec<u8>, String> {
        let mut durable: DurableV2 =
            postcard::from_bytes(&self.durable_v2_for_test()?).map_err(err)?;
        for document in &mut durable.documents {
            document.covered_snapshot = None;
        }
        postcard::to_allocvec(&durable).map_err(err)
    }

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
        let (durable, pending, pending_bytes) = match version {
            2 => {
                let old: DurableV2 = postcard::from_bytes(bytes).map_err(err)?;
                (Durable {
                    version: 3,
                    room: old.room,
                    authority: old.authority,
                    room_bytes: old.room_bytes,
                    documents: old
                        .documents
                        .into_iter()
                        .map(|document| DurableDocument {
                            route: document.route,
                            bytes: document.bytes,
                            watermarks: document.watermarks,
                            wire_version: None,
                            encryption_required: document.encryption_required,
                            covered: document.covered,
                            covered_snapshot: document.covered_snapshot,
                        })
                        .collect(),
                    receipts: old.receipts,
                    certificates: old.certificates,
                    revocations: old.revocations,
                }, VecDeque::new(), 0)
            }
            3 => (postcard::from_bytes(bytes).map_err(err)?, VecDeque::new(), 0),
            4 => {
                let value: DurableV4 = postcard::from_bytes(bytes).map_err(err)?;
                (value.base, value.pending, value.pending_bytes)
            }
            5 => {
                let (value, tail): (DurableV5<'_>, _) = postcard::take_from_bytes(bytes).map_err(err)?;
                if !tail.is_empty() || value.version != 5 || value.base.version != 4 {
                    return Err("invalid native v5 durable envelope".into());
                }
                exact_admissions = ExactAdmissions::decode(value.exact).map_err(err)?;
                (value.base.base, value.base.pending, value.base.pending_bytes)
            }
            0 | 1 => {
                return Err("incomplete: legacy admission lacks durable identity/revocation prerequisites; preserve bytes for authenticated migration".into());
            }
            _ => return Err("incomplete: unsupported durable admission version; preserve bytes for authenticated migration".into()),
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
                certificates: verified_certificates,
                revocations: RevocationList::new(),
                receipts: BTreeMap::new(),
                room_doc: None,
                documents: BTreeMap::new(),
                pending,
                pending_bytes,
                checkpoint_times: BTreeMap::new(),
                exact_admissions,
                admission_v5: version == 5,
                native_cut_publisher: None,
                retention: (),
            },
        };
        backend::validate_pending(&peer.core.pending, peer.core.pending_bytes).map_err(err)?;
        peer.core.revocations = backend::restore_revocation_union(
            durable.revocations,
            &revocations,
            &peer.core.certificates,
            peer.core.authority.device_public_key,
        )
        .map_err(err)?;
        peer.core.receipts = durable.receipts;
        peer.core.room_doc = durable
            .room_bytes
            .map(|bytes| {
                AutoCommit::load(&bytes)
                    .map(CachedDocument::from)
                    .map_err(err)
            })
            .transpose()?;
        for document in durable.documents {
            let governed_version = governed_snapshot_wire_version(
                peer.core.room_doc.as_deref(),
                &document.route,
                document.covered_snapshot.as_deref(),
            )?;
            if document
                .wire_version
                .zip(governed_version)
                .is_some_and(|(stored, governed)| stored != governed)
            {
                return Err(format!(
                    "durable document wire version conflicts with governed checkpoint for {}/{}",
                    document.route.app, document.route.record
                ));
            }
            let wire_version = document.wire_version.or(governed_version);
            let mut channel = ChannelState::default();
            channel.last_seq_per_author = document
                .watermarks
                .into_iter()
                .map(|(key, seq)| (Ed25519PublicKey::from_bytes(key), seq))
                .collect();
            peer.core.documents.insert(
                document.route,
                Document {
                    doc: AutoCommit::load(&document.bytes).map_err(err)?,
                    channel,
                    wire_version,
                    encryption_required: document.encryption_required,
                    covered: document.covered,
                    covered_snapshot: document
                        .covered_snapshot
                        .map(|bytes| AcceptedCheckpoint::new(bytes, wire_version)),
                },
            );
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
        for (key, _) in peer.core.exact_admissions.iter() {
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
#[derive(Serialize, Deserialize)]
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
}
impl GovernedIdentity {
    pub(crate) fn has_admission(&self) -> bool {
        self.admission.is_some() || !self.admissions.is_empty()
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

fn member_role(role: crate::join::MemberRole) -> Role {
    match role {
        crate::join::MemberRole::Viewer => Role::Viewer,
        crate::join::MemberRole::Commenter => Role::Commenter,
        crate::join::MemberRole::Editor => Role::Editor,
    }
}

fn member_role_from_role(role: Role) -> Option<crate::join::MemberRole> {
    match role {
        Role::Viewer => Some(crate::join::MemberRole::Viewer),
        Role::Commenter => Some(crate::join::MemberRole::Commenter),
        Role::Editor => Some(crate::join::MemberRole::Editor),
        Role::Owner | Role::Denied => None,
    }
}

fn member_verb(role: crate::join::MemberRole) -> Verb {
    match role {
        crate::join::MemberRole::Viewer => Verb::Read,
        crate::join::MemberRole::Commenter => Verb::Comment,
        crate::join::MemberRole::Editor => Verb::Write,
    }
}

fn member_capability_id(user_public_key: [u8; 32]) -> String {
    format!("room-member-v1:{}", hex::encode(user_public_key))
}

pub(crate) fn put_member_capability(
    doc: &mut AutoCommit,
    capabilities: &automerge::ObjId,
    user_public_key: [u8; 32],
    role: crate::join::MemberRole,
) -> Result<(), String> {
    let id = member_capability_id(user_public_key);
    let person = hex::encode(user_public_key);
    let shadowing = doc
        .keys(capabilities)
        .filter(|key| {
            doc.get(capabilities, key)
                .ok()
                .flatten()
                .is_some_and(|(value, _)| {
                    let automerge::Value::Scalar(value) = value else {
                        return false;
                    };
                    let automerge::ScalarValue::Bytes(bytes) = value.as_ref() else {
                        return false;
                    };
                    serde_json::from_slice::<CapOverride>(bytes).is_ok_and(|cap| {
                        subject_is_person(&cap.subject, user_public_key, &person)
                            && matches!(cap.scope, Scope::Room(_) | Scope::Workspace)
                    })
                })
        })
        .collect::<Vec<_>>();
    for key in shadowing {
        doc.delete(capabilities, key).map_err(err)?;
    }
    let cap = CapOverride::grant(
        Subject::User(Ed25519PublicKey::from_bytes(user_public_key)),
        Scope::Workspace,
        member_verb(role),
    )
    .with_id(id.clone());
    doc.put(
        capabilities,
        id,
        automerge::ScalarValue::Bytes(serde_json::to_vec(&cap).map_err(err)?),
    )
    .map_err(err)
}

fn subject_is_person(subject: &Subject, root: [u8; 32], canonical: &str) -> bool {
    match subject {
        Subject::User(key) => key.as_bytes() == &root,
        Subject::Principal(Principal::Person(person)) => person == canonical,
        _ => false,
    }
}
/// The same native room CRDT keys projected by WorkspaceTree::sync_room_doc.
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
    doc.put_object(ROOT, "capabilities", ObjType::Map)
        .map_err(err)?;
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
            .receive_from_report(origin, snapshot.clone(), later)
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
        let literal = include_bytes!("../tests/fixtures/delivery-admission/native-v5.postcard");
        let (peer, _, _, _, now) = crate::admission_tests::seeded();
        let value: DurableV5<'_> = postcard::from_bytes(literal).unwrap();
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
        let value: DurableV5<'_> = postcard::from_bytes(literal).unwrap();
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
