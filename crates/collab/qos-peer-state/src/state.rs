//! Certified room and document replica. Transport identity is always an input.
use crate::cached_document::CachedDocument;
use crate::verified_certificate::VerifiedCertificate;
use crate::{NeighbourMsg, document, room};
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, transaction::Transactable};
use collab_types::caps::{Actor, CapDecision, Principal, ResourceRef, decide_capability};
use collab_types::{
    ArtifactKind, ChannelState, DocumentMutation, DocumentWireVersion, Ed25519PublicKey, OpClass,
    Role, SignedOp,
};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use tilo_user_identity::{DeviceCert, RevocationList};
use web_time::{Duration, SystemTime};
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
#[derive(Clone, Debug, Default)]
pub struct ReceiveOutcome {
    pub changed: bool,
    pub deferred: bool,
    pub duplicate: bool,
    pub rejected: usize,
    pub metadata_changed: bool,
    pub incomplete: Option<String>,
}
/// Only the relay Delta envelope authenticates an attributed state author.
#[derive(Clone, Copy, Debug)]
pub enum TransportOrigin {
    RelayDelta([u8; 32]),
    RetainedRelayDelta([u8; 32]),
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

#[derive(Clone)]
struct Document {
    doc: AutoCommit,
    channel: ChannelState,
    wire_version: Option<DocumentWireVersion>,
    encryption_required: bool,
    covered: BTreeMap<String, u64>,
    covered_snapshot: Option<AcceptedCheckpoint>,
}
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
    room: String,
    authority: DeviceCert,
    certificates: BTreeMap<[u8; 32], VerifiedCertificate>,
    revocations: RevocationList,
    receipts: BTreeMap<String, Vec<u8>>,
    room_doc: Option<CachedDocument>,
    documents: BTreeMap<Route, Document>,
    pending: VecDeque<([u8; 32], NeighbourMsg, bool)>,
    pending_bytes: usize,
    checkpoint_times: BTreeMap<Route, SystemTime>,
}

fn governed_snapshot_wire_version(
    room: Option<&AutoCommit>,
    route: &Route,
    snapshot: Option<&[u8]>,
) -> Result<Option<DocumentWireVersion>, String> {
    let Some(snapshot) = snapshot else {
        return Ok(None);
    };
    let Some(coverage) = room
        .map(crate::coverage::entries)
        .transpose()?
        .and_then(|entries| {
            entries
                .get(&crate::coverage::route_key(&route.app, &route.record))
                .cloned()
        })
    else {
        return Ok(None);
    };
    let mutation = DocumentMutation::Snapshot(snapshot.to_vec());
    let matches = DocumentWireVersion::ALL
        .into_iter()
        .filter(|version| {
            mutation
                .encode_for(*version)
                .is_ok_and(|payload| blake3::hash(&payload).as_bytes() == &coverage.payload_hash)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(format!(
            "governed checkpoint for {}/{} does not match a supported document wire version; preserve journal and request a fresh authenticated checkpoint",
            route.app, route.record
        )),
        [version] => Ok(Some(*version)),
        _ => Err(format!(
            "governed checkpoint for {}/{} ambiguously matches multiple document wire versions",
            route.app, route.record
        )),
    }
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
        if key.verifying_key().to_bytes() != self.authority.device_public_key {
            return Err("governance writer is not pinned authority".into());
        }
        self.certificate(&self.authority.device_public_key, now)?;
        if cert.user_public_key != self.authority.user_public_key {
            return Err("enrollment root mismatch".into());
        }
        tilo_user_identity::verify_device_cert_at(&cert, now_ns(now)).map_err(err)?;
        if self.revocations.is_revoked(&cert.device_public_key) {
            return Err("device revoked".into());
        }
        if let Some(old) = self.certificates.get(&cert.device_public_key) {
            if old.as_ref() != &cert {
                return Err("certificate conflict".into());
            }
        }
        let mut doc = self.room_doc.clone().ok_or("room governance missing")?;
        let identity = GovernedIdentity {
            device_cert: cert.clone(),
            revocations: vec![],
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
        let changes = doc
            .get_changes(&[])
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        let room = self.room.clone();
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
        let mut changed = false;
        let mut certificates = self.certificates.clone();
        let mut revocations = self.revocations.clone();
        for (id, bytes) in room::governance_of(Some(doc)).3 {
            let identity: GovernedIdentity = serde_json::from_slice(&bytes).map_err(err)?;
            let cert = identity.device_cert;
            if id != hex::encode(cert.device_public_key) {
                return Err("governed identity key mismatch".into());
            }
            let verified = if let Some(known) = certificates.get(&cert.device_public_key) {
                if known.as_ref() != &cert {
                    return Err("certificate conflict".into());
                }
                known.clone()
            } else {
                VerifiedCertificate::new(cert.clone(), now_ns(now))?
            };
            // Existing roots remain supported; a new enrollment cannot expand
            // trust beyond the pinned authority's root.
            if !self.certificates.contains_key(&cert.device_public_key)
                && cert.user_public_key != self.authority.user_public_key
            {
                return Err("enrollment root mismatch".into());
            }
            for entry in identity.revocations {
                if entry.user_public_key != cert.user_public_key {
                    return Err("revocation root mismatch".into());
                }
                if certificates
                    .get(&entry.revoked_device_public_key)
                    .is_some_and(|target| target.user_public_key != entry.user_public_key)
                {
                    return Err("revocation cannot cross identity roots".into());
                }
                changed |= revocations.insert(entry).map_err(err)?;
            }
            changed |= certificates.get(&cert.device_public_key).map(AsRef::as_ref) != Some(&cert);
            certificates.insert(cert.device_public_key, verified);
        }
        self.certificates = certificates;
        self.revocations = revocations;
        Ok(changed)
    }
    /// Verified public identity metadata; never includes signing keys.
    pub fn certificates(&self) -> Vec<DeviceCert> {
        self.certificates
            .values()
            .map(|cert| cert.as_ref().clone())
            .collect()
    }
    pub fn revocations(&self) -> Vec<tilo_user_identity::RevocationEntry> {
        let mut entries = self.revocations.iter().cloned().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.revoked_device_public_key);
        entries
    }
    pub fn replay_floors(&self) -> Vec<(String, String, [u8; 32], u64)> {
        let mut rows = self
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
        self.checkpoint_times
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
        &self.authority
    }
    pub fn room(&self) -> &str {
        &self.room
    }
    pub fn permissions(
        &self,
        source: [u8; 32],
        now: SystemTime,
    ) -> Result<(Role, Vec<collab_types::caps::CapOverride>, String), String> {
        let role = self.role(&source, now)?;
        let cert = self.certificate(&source, now)?;
        let caps = self
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
        self.room_doc.as_deref()
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
        let actor = Actor::person(Some(role), &person)
            .with_principal(Principal::App(app.into()))
            .in_room(self.room.clone());
        let resource = ResourceRef::Artifact {
            app: Some(app.into()),
            kind: ArtifactKind::Note,
            instance: collab_types::document_artifact_id(app, &self.room, record),
        };
        if decide_capability(
            &actor,
            &resource,
            collab_types::caps::Verb::Read,
            &caps,
            Some(now_secs(now)),
        ) != CapDecision::Allow
        {
            return Err(format!("record read forbidden: {app}/{record}"));
        }
        Ok(())
    }
    pub fn has_governance(&self) -> bool {
        self.room_doc.is_some()
    }
    pub fn incomplete_reason(&self) -> Option<String> {
        self.documents.values().any(|entry|entry.encryption_required).then(||"incomplete: authenticated persisted MLS group required; plaintext downgrade forbidden".into())
    }
    pub fn pending_count(&self) -> usize {
        self.pending.len()
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
        if cluster_id != &self.room {
            return false;
        }
        self.documents
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
        if cluster_id != &self.room {
            return false;
        }
        let Some(document) = self.documents.get(&Route {
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
        self.documents
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
        self.documents
            .get(&Route {
                app: app.into(),
                record: record.into(),
            })
            .and_then(|document| document.wire_version)
    }
    /// Read the admitted document basis without serializing its contents.
    pub fn document_heads(&self, app: &str, record: &str) -> Option<Vec<automerge::ChangeHash>> {
        self.documents
            .get(&Route {
                app: app.into(),
                record: record.into(),
            })
            .map(|entry| entry.doc.base_heads())
    }
    pub fn document(&self, app: &str, record: &str) -> Option<Vec<u8>> {
        self.documents
            .get(&Route {
                app: app.into(),
                record: record.into(),
            })
            .map(|entry| entry.doc.clone().save())
    }
    fn certificate(&self, source: &[u8; 32], now: SystemTime) -> Result<&DeviceCert, String> {
        let cert = self
            .certificates
            .get(source)
            .ok_or("certified identity missing")?;
        let cert = cert.current(now_ns(now))?;
        if self.revocations.is_revoked(source) {
            return Err("device revoked".into());
        }
        Ok(cert)
    }
    fn role(&self, source: &[u8; 32], now: SystemTime) -> Result<Role, String> {
        let cert = self.certificate(source, now)?;
        let policy = self
            .room_doc
            .as_ref()
            .ok_or("room governance missing")?
            .policy();
        // A root-signed certificate must also be the certificate admitted by room governance.
        let identity: GovernedIdentity = serde_json::from_slice(
            policy
                .identities
                .get(&hex::encode(source))
                .ok_or("device is absent from governed roster")?,
        )
        .map_err(err)?;
        if identity.device_cert != *cert {
            return Err("governed certificate mismatch".into());
        }
        let mut revoked = self.revocations.clone();
        for entry in identity.revocations {
            if entry.user_public_key != cert.user_public_key {
                return Err("revocation root mismatch".into());
            }
            revoked.insert(entry).map_err(err)?;
        }
        if revoked.is_revoked(source) {
            return Err("device revoked by room".into());
        }
        let person = Principal::person_from_key(Ed25519PublicKey::from_bytes(cert.user_public_key));
        room::role_of(
            policy.owner.as_deref(),
            &policy.caps,
            person.id(),
            now_secs(now),
        )
        .ok_or("person has no room membership".into())
    }
    pub fn receive_from(
        &mut self,
        origin: TransportOrigin,
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<ReceiveOutcome, String> {
        let (source, retained_replay) = match origin {
            TransportOrigin::RelayDelta(source) => (source, false),
            TransportOrigin::RetainedRelayDelta(source) => (source, true),
            TransportOrigin::UntrustedPresence(source) => {
                if matches!(
                    message,
                    NeighbourMsg::SyncResp { .. } | NeighbourMsg::AppDocument { .. }
                ) {
                    return Err("state requires relay-authenticated Delta carrier".into());
                }
                (source, false)
            }
        };
        let mut candidate = self.clone();
        let outcome = candidate.receive_with_context(source, message, now, retained_replay)?;
        *self = candidate;
        Ok(outcome)
    }
    pub(crate) fn receive(
        &mut self,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<ReceiveOutcome, String> {
        self.receive_with_context(source, message, now, false)
    }
    fn receive_with_context(
        &mut self,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
        retained_replay: bool,
    ) -> Result<ReceiveOutcome, String> {
        match message {
            NeighbourMsg::IdentityHello { cert, revocations } => {
                if cert.device_public_key != source {
                    return Err("transport certificate mismatch".into());
                }
                if self.certificates.get(&source).map(AsRef::as_ref) != Some(cert.as_ref()) {
                    let admitted = self
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
                    self.certificates.get(&source).map(AsRef::as_ref) != Some(cert.as_ref());
                for entry in revocations {
                    if entry.user_public_key != cert.user_public_key {
                        return Err("revocation root mismatch".into());
                    }
                    if self
                        .certificates
                        .get(&entry.revoked_device_public_key)
                        .is_some_and(|target| target.user_public_key != entry.user_public_key)
                    {
                        return Err("revocation cannot cross identity roots".into());
                    }
                    metadata_changed |= self.revocations.insert(entry).map_err(err)?;
                }
                if self.revocations.is_revoked(&source) {
                    return Err("device revoked".into());
                }
                self.certificates.insert(source, verified);
                Ok(ReceiveOutcome {
                    metadata_changed,
                    ..Default::default()
                })
            }
            NeighbourMsg::SyncResp {
                changes,
                room: received,
            } => {
                if received != self.room {
                    return Err("room topic mismatch".into());
                }
                self.certificate(&source, now)?;
                if self.room_doc.is_some() || source != self.authority.device_public_key {
                    let role = self.role(&source, now)?;
                    let certificate = self.certificate(&source, now)?;
                    let person = Principal::person_from_key(Ed25519PublicKey::from_bytes(
                        certificate.user_public_key,
                    ));
                    let (_, _, caps) = room::extract_governed_fields(
                        self.room_doc.as_deref().ok_or("room governance missing")?,
                    );
                    let actor = Actor::person(Some(role), person.id()).in_room(self.room.clone());
                    if decide_capability(
                        &actor,
                        &ResourceRef::Room(self.room.clone()),
                        collab_types::caps::Verb::Write,
                        &caps,
                        Some(now_secs(now)),
                    ) != CapDecision::Allow
                    {
                        return Err("room write forbidden".into());
                    }
                }
                let candidate = room::candidate(self.room_doc.as_deref(), &changes).map_err(err)?;
                if room::declared_room_mismatches(
                    &candidate,
                    uuid::Uuid::parse_str(&self.room).map_err(err)?,
                ) {
                    return Err("declared room mismatch".into());
                }
                let pinned = hex::encode(self.authority.device_public_key);
                let from = hex::encode(source);
                if !room::governance_change_allowed(
                    self.room_doc.as_deref(),
                    &candidate,
                    false,
                    Some(&pinned),
                    &from,
                ) {
                    return Err("governance writer is not pinned authority".into());
                }
                crate::topology::validate(self.room_doc.as_deref(), &candidate, &from, &pinned)?;
                if !room::immutable_receipts(self.room_doc.as_deref(), &candidate) {
                    return Err("effect receipt is immutable".into());
                }
                let mut receipts = self.receipts.clone();
                for (key, value) in room::receipt_values(&candidate) {
                    let bytes = serde_json::to_vec(&value).map_err(err)?;
                    if receipts.get(&key).is_some_and(|prior| prior != &bytes) {
                        return Err("effect receipt is immutable".into());
                    }
                    receipts.insert(key, bytes);
                }
                crate::coverage::monotonic(self.room_doc.as_deref(), &candidate)?;
                crate::durability::monotonic(self.room_doc.as_deref(), &candidate)?;
                let changed = self
                    .room_doc
                    .as_deref()
                    .is_none_or(|doc| doc.base_heads() != candidate.base_heads());
                let mut metadata_changed = self.import_governed_certificates(&candidate, now)?;
                self.receipts = receipts;
                self.room_doc = Some(candidate.into());
                let pending = std::mem::take(&mut self.pending);
                self.pending_bytes = 0;
                let mut applied = changed;
                let mut rejected = 0;
                for (author, message, replayed) in pending {
                    match self.receive_with_context(author, message, now, replayed) {
                        Ok(outcome) => {
                            applied |= outcome.changed;
                            metadata_changed |= outcome.metadata_changed;
                            rejected += outcome.rejected;
                        }
                        Err(_) => rejected += 1,
                    }
                }
                Ok(ReceiveOutcome {
                    changed: applied,
                    metadata_changed,
                    incomplete: self.incomplete_reason(),
                    deferred: !self.pending.is_empty(),
                    rejected,
                    ..Default::default()
                })
            }
            message @ NeighbourMsg::AppDocument { .. } => {
                if self.room_doc.is_none() || !self.certificates.contains_key(&source) {
                    return self.defer(source, message, retained_replay);
                }
                self.receive_document(source, message, now, retained_replay)
            }
            _ => Ok(ReceiveOutcome::default()),
        }
    }
    fn defer(
        &mut self,
        source: [u8; 32],
        message: NeighbourMsg,
        retained_replay: bool,
    ) -> Result<ReceiveOutcome, String> {
        let bytes = postcard::to_allocvec(&message).map_err(err)?;
        if self.pending.iter().any(|(key, prior, replayed)| {
            key == &source && *prior == message && *replayed == retained_replay
        }) {
            return Ok(ReceiveOutcome {
                deferred: true,
                ..Default::default()
            });
        }
        if self.pending.len() >= 64 || bytes.len() > 4 * 1024 * 1024usize - self.pending_bytes {
            return Err("app prerequisite queue full; fresh checkpoint required".into());
        }
        self.pending_bytes += bytes.len();
        self.pending.push_back((source, message, retained_replay));
        Ok(ReceiveOutcome {
            deferred: true,
            ..Default::default()
        })
    }
    fn receive_document(
        &mut self,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
        retained_replay: bool,
    ) -> Result<ReceiveOutcome, String> {
        self.admit_document(source, message, now, false, retained_replay)
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
                cluster_id: self.room.clone(),
                record_id: record.into(),
                envelope: Box::new(envelope),
            },
            now,
            true,
            false,
        )
    }

    fn admit_document(
        &mut self,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
        local_app: bool,
        retained_replay: bool,
    ) -> Result<ReceiveOutcome, String> {
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
        if cluster_id != self.room
            || peer != peer_handle(&source)
            || envelope.author.as_bytes() != &source
        {
            return Err("transport author or document room mismatch".into());
        }
        let authorization_time = if retained_replay {
            envelope.timestamp
        } else {
            now
        };
        let role = self.role(&source, authorization_time)?;
        let cert = self.certificate(&source, authorization_time)?;
        let (_, _, caps) =
            room::extract_governed_fields(self.room_doc.as_deref().ok_or("missing governance")?);
        let person = Principal::person_from_key(Ed25519PublicKey::from_bytes(cert.user_public_key));
        let actor = Actor::person(Some(role), person.id())
            .with_principal(Principal::App(app_id.clone()))
            .in_room(self.room.clone());
        let resource = ResourceRef::Artifact {
            app: Some(app_id.clone()),
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
        let route_existed = self.documents.contains_key(&route);
        let mut candidate = self
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
            if retained_replay {
                Duration::ZERO
            } else {
                Duration::from_secs(300)
            },
        ) {
            Ok(()) => {}
            Err(collab_types::ValidationError::ReplaySeq { .. }) => {
                return Ok(ReceiveOutcome {
                    duplicate: true,
                    ..Default::default()
                });
            }
            Err(error) => return Err(err(error)),
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
                self.documents.insert(route,candidate);
                return Ok(ReceiveOutcome{metadata_changed:true,incomplete:self.incomplete_reason(),..Default::default()});
            },
            _=>return Err("incomplete: authenticated persisted MLS group required; plaintext downgrade forbidden".into()),
        };
        candidate.channel = validated_channel;
        if let DocumentMutation::Snapshot(bytes) = &mutation
            && !local_app
        {
            let entries =
                crate::coverage::entries(self.room_doc.as_deref().ok_or("missing room")?)?;
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
                );
            }
            let snapshot = AutoCommit::load(bytes).map_err(err)?;
            let heads = snapshot
                .base_heads()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            if coverage.room != self.room
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
            );
        } else if candidate.wire_version.is_none() {
            return Err(
                "document wire version unavailable; fresh authenticated checkpoint required".into(),
            );
        }
        let changed = document::merge_committed(&mut candidate.doc, &mutation)
            .map_err(|e| format!("{e:?}"))?;
        if local_app && !candidate.doc.get_missing_deps(&[]).is_empty() {
            return Err("local publication has unresolved document dependencies".into());
        }
        if !local_app && matches!(mutation, DocumentMutation::Snapshot(_)) {
            self.checkpoint_times.insert(route.clone(), now);
        }
        self.documents.insert(route, candidate);
        Ok(ReceiveOutcome {
            changed,
            metadata_changed: true,
            ..Default::default()
        })
    }
    pub fn room_message(&self) -> NeighbourMsg {
        let changes = self
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
            room: self.room.clone(),
        }
    }
    pub fn install_seed(
        &mut self,
        seed: SeedState,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        if key.verifying_key().to_bytes() != self.authority.device_public_key {
            return Err("only pinned authority may initialize fixture state".into());
        }
        self.receive(
            key.verifying_key().to_bytes(),
            NeighbourMsg::SyncResp {
                changes: seed.room_changes,
                room: self.room.clone(),
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
        let author = Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        let wire_version = match self.documents.get(&route) {
            Some(document) => document.wire_version.ok_or(
                "document wire version unavailable; fresh authenticated checkpoint required",
            )?,
            None => DocumentWireVersion::CURRENT,
        };
        let seq = self
            .documents
            .get(&route)
            .and_then(|entry| entry.channel.last_seq_per_author.get(&author))
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("sequence exhausted")?;
        let mut op = SignedOp::new_unsigned(
            mutation.encode_for(wire_version).map_err(err)?,
            collab_types::document_artifact_id_for_version(wire_version, app, &self.room, record),
            author,
            collab_types::document_attestation_id_for_version(
                wire_version,
                app,
                &self.room,
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
            cluster_id: self.room.clone(),
            record_id: record.into(),
            envelope: Box::new(op),
        };
        if let DocumentMutation::Snapshot(bytes) = &mutation {
            if author.as_bytes() != &self.authority.device_public_key {
                return Err("only pinned governor may publish a retained checkpoint".into());
            }
            let snapshot = AutoCommit::load(bytes).map_err(err)?;
            let mut watermarks = self
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
            let coverage = crate::coverage::Coverage {
                room: self.room.clone(),
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
            };
            crate::coverage::put(
                self.room_doc
                    .as_deref_mut()
                    .ok_or("missing room governance")?,
                coverage,
            )?;
        }
        self.receive(*author.as_bytes(), message.clone(), now)?;
        Ok(message)
    }
    pub fn signed_snapshots(
        &mut self,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let documents = self
            .documents
            .iter()
            .map(|(route, entry)| (route.clone(), entry.doc.clone().save()))
            .collect::<Vec<_>>();
        documents
            .into_iter()
            .map(|(route, bytes)| self.publish_document(&route.app, &route.record, bytes, key, now))
            .collect()
    }
    /// The governor publishes one consistent room coverage/checkpoint pair.
    pub fn bootstrap_messages(
        &mut self,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let mut candidate = self.clone();
        let snapshots = candidate.signed_snapshots(key, now)?;
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
        let snapshots = candidate.signed_snapshots(key, now)?;
        let room = candidate
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
        crate::durability::monotonic(self.room_doc.as_deref(), room)?;
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
        if self.room != before.room || self.authority != before.authority {
            return Err("publication cannot replace the session namespace".into());
        }
        if key.verifying_key().to_bytes() != self.authority.device_public_key {
            return Err("publication requires pinned authority".into());
        }
        self.permissions(key.verifying_key().to_bytes(), now)?;
        if self.incomplete_reason().is_some() {
            return Err("cannot publish incomplete encryption state".into());
        }
        let before_room = before.room_doc.as_ref().ok_or("missing prior governance")?;
        let mut candidate = self.clone();
        for (route, prior) in &before.documents {
            let document = candidate
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
        for (route, document) in &candidate.documents {
            let route_key = crate::coverage::route_key(&route.app, &route.record);
            let changed = before.documents.get(route).is_none_or(|prior| {
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
                room: self.room.clone(),
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
        let Some(room) = self.room_doc.as_deref() else {
            return Ok(None);
        };
        let receipts = crate::durability::entries(room)?;
        let Some(receipt) = receipts.get(&crate::coverage::route_key(app, record)) else {
            return Ok(None);
        };
        let coverage = &receipt.coverage;
        if coverage.room != self.room
            || coverage.app != app
            || coverage.record != record
            || coverage.author != self.authority.device_public_key
            || self.revocations.is_revoked(&coverage.author)
        {
            return Err("durable receipt authority/route mismatch".into());
        }
        let Some(document) = self.documents.get(&Route {
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
        postcard::to_allocvec(&Durable {
            version: 3,
            room: self.room.clone(),
            authority: self.authority.clone(),
            room_bytes: self.room_doc.as_ref().map(CachedDocument::saved),
            documents,
            receipts: self.receipts.clone(),
            certificates: self
                .certificates
                .values()
                .map(|cert| cert.as_ref().clone())
                .collect(),
            revocations: {
                let mut entries = self.revocations.iter().cloned().collect::<Vec<_>>();
                entries.sort_by_key(|entry| entry.revoked_device_public_key);
                entries
            },
        })
        .map_err(err)
    }

    #[cfg(test)]
    pub(crate) fn durable_v2_for_test(&self) -> Result<Vec<u8>, String> {
        let documents = self
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
            room: self.room.clone(),
            authority: self.authority.clone(),
            room_bytes: self.room_doc.as_ref().map(CachedDocument::saved),
            documents,
            receipts: self.receipts.clone(),
            certificates: self
                .certificates
                .values()
                .map(|certificate| certificate.as_ref().clone())
                .collect(),
            revocations: {
                let mut entries = self.revocations.iter().cloned().collect::<Vec<_>>();
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
        let durable = match version {
            2 => {
                let old: DurableV2 = postcard::from_bytes(bytes).map_err(err)?;
                Durable {
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
                }
            }
            3 => postcard::from_bytes(bytes).map_err(err)?,
            0 | 1 => {
                return Err("incomplete: legacy admission lacks durable identity/revocation prerequisites; preserve bytes for authenticated migration".into());
            }
            _ => return Err("incomplete: unsupported durable admission version; preserve bytes for authenticated migration".into()),
        };
        if durable.version != 3 || durable.room != room || durable.authority != authority {
            return Err("durable admission namespace mismatch".into());
        }
        let mut certificates = durable.certificates;
        for certificate in roster {
            if certificates.iter().any(|prior| {
                prior.device_public_key == certificate.device_public_key && prior != &certificate
            }) {
                return Err("durable certificate conflicts with enrollment".into());
            }
            certificates.push(certificate);
        }
        let mut peer = Self::new(room, authority, certificates, RevocationList::new(), now)?;
        for entry in durable
            .revocations
            .into_iter()
            .chain(revocations.iter().cloned())
        {
            if peer
                .certificates
                .get(&entry.revoked_device_public_key)
                .is_some_and(|certificate| certificate.user_public_key != entry.user_public_key)
            {
                return Err("durable revocation root mismatch".into());
            }
            peer.revocations.insert(entry).map_err(err)?;
        }
        peer.receipts = durable.receipts;
        peer.room_doc = durable
            .room_bytes
            .map(|bytes| {
                AutoCommit::load(&bytes)
                    .map(CachedDocument::from)
                    .map_err(err)
            })
            .transpose()?;
        for document in durable.documents {
            let governed_version = governed_snapshot_wire_version(
                peer.room_doc.as_deref(),
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
            peer.documents.insert(
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
        Ok(peer)
    }
}
#[derive(Serialize, Deserialize)]
pub(crate) struct GovernedIdentity {
    pub(crate) device_cert: DeviceCert,
    #[serde(default)]
    pub(crate) revocations: Vec<tilo_user_identity::RevocationEntry>,
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
