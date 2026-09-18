//! Certified room and document replica. Transport identity is always an input.
use crate::{NeighbourMsg, document, room};
use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable};
use collab_types::caps::{Actor, CapDecision, Principal, ResourceRef, decide_capability};
use collab_types::{
    ArtifactKind, ChannelState, DocumentMutation, Ed25519PublicKey, OpClass, Role, SignedOp,
};
use ed25519_dalek::SigningKey;
use qos_user_identity::{DeviceCert, RevocationList};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
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
    UntrustedPresence([u8; 32]),
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct Route {
    app: String,
    record: String,
}
#[derive(Clone)]
struct Document {
    doc: AutoCommit,
    channel: ChannelState,
    encryption_required: bool,
    covered: BTreeMap<String, u64>,
    covered_snapshot: Option<Vec<u8>>,
}
#[derive(Serialize, Deserialize)]
struct DurableDocument {
    route: Route,
    bytes: Vec<u8>,
    watermarks: Vec<([u8; 32], u64)>,
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
    revocations: Vec<qos_user_identity::RevocationEntry>,
}
#[derive(Clone)]
pub struct StatePeer {
    room: String,
    authority: DeviceCert,
    certificates: BTreeMap<[u8; 32], DeviceCert>,
    revocations: RevocationList,
    receipts: BTreeMap<String, Vec<u8>>,
    room_doc: Option<AutoCommit>,
    documents: BTreeMap<Route, Document>,
    pending: VecDeque<([u8; 32], NeighbourMsg)>,
    pending_bytes: usize,
    checkpoint_times: BTreeMap<Route, SystemTime>,
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
            qos_user_identity::verify_device_cert_at(&certificate, now_ns(now)).map_err(err)?;
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
    /// Verified public identity metadata; never includes signing keys.
    pub fn certificates(&self) -> Vec<DeviceCert> {
        self.certificates.values().cloned().collect()
    }
    pub fn revocations(&self) -> Vec<qos_user_identity::RevocationEntry> {
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
        let (_, _, caps) =
            room::extract_governed_fields(self.room_doc.as_ref().ok_or("room missing")?);
        let person = Principal::person_from_key(Ed25519PublicKey::from_bytes(cert.user_public_key));
        Ok((role, caps, person.id().to_owned()))
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
    pub fn covers(&self, message: &NeighbourMsg) -> bool {
        let NeighbourMsg::AppDocument {
            app_id,
            record_id,
            envelope,
            ..
        } = message
        else {
            return false;
        };
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
        let Ok(collab_types::RoutedDocumentPayload::Cleartext(DocumentMutation::Incremental(
            bytes,
        ))) =
            collab_types::validate_document_payload_route(app_id, &self.room, record_id, envelope)
        else {
            return false;
        };
        let Ok(mut proof) = AutoCommit::load(snapshot) else {
            return false;
        };
        let before = proof.base_heads();
        // A sequence floor is not causal coverage: an out-of-order incremental
        // may have been validated while its dependencies were unavailable.
        proof.load_incremental(&bytes).is_ok()
            && proof.get_missing_deps(&[]).is_empty()
            && proof.base_heads() == before
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
        qos_user_identity::verify_device_cert_at(cert, now_ns(now)).map_err(err)?;
        if self.revocations.is_revoked(source) {
            return Err("device revoked".into());
        }
        Ok(cert)
    }
    fn role(&self, source: &[u8; 32], now: SystemTime) -> Result<Role, String> {
        let cert = self.certificate(source, now)?;
        let doc = self.room_doc.as_ref().ok_or("room governance missing")?;
        // A root-signed certificate must also be the certificate admitted by room governance.
        let (_, _, _, identities, _) = room::governance_of(Some(doc));
        let identity: GovernedIdentity = serde_json::from_slice(
            identities
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
        let (_, owner, caps) = room::extract_governed_fields(doc);
        let person = Principal::person_from_key(Ed25519PublicKey::from_bytes(cert.user_public_key));
        room::role_of(owner.as_deref(), &caps, person.id(), now_secs(now))
            .ok_or("person has no room membership".into())
    }
    pub fn receive_from(
        &mut self,
        origin: TransportOrigin,
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<ReceiveOutcome, String> {
        let source = match origin {
            TransportOrigin::RelayDelta(source) => source,
            TransportOrigin::UntrustedPresence(source) => {
                if matches!(
                    message,
                    NeighbourMsg::SyncResp { .. } | NeighbourMsg::AppDocument { .. }
                ) {
                    return Err("state requires relay-authenticated Delta carrier".into());
                }
                source
            }
        };
        let mut candidate = self.clone();
        let outcome = candidate.receive(source, message, now)?;
        *self = candidate;
        Ok(outcome)
    }
    pub(crate) fn receive(
        &mut self,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<ReceiveOutcome, String> {
        match message {
            NeighbourMsg::IdentityHello { cert, revocations } => {
                if cert.device_public_key != source {
                    return Err("transport certificate mismatch".into());
                }
                if self.certificates.get(&source) != Some(cert.as_ref()) {
                    let admitted = self
                        .room_doc
                        .as_ref()
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
                qos_user_identity::verify_device_cert_at(&cert, now_ns(now)).map_err(err)?;
                let mut metadata_changed = self.certificates.get(&source) != Some(cert.as_ref());
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
                self.certificates.insert(source, *cert);
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
                        self.room_doc.as_ref().ok_or("room governance missing")?,
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
                let candidate = room::candidate(self.room_doc.as_ref(), &changes).map_err(err)?;
                if room::declared_room_mismatches(
                    &candidate,
                    uuid::Uuid::parse_str(&self.room).map_err(err)?,
                ) {
                    return Err("declared room mismatch".into());
                }
                let pinned = hex::encode(self.authority.device_public_key);
                let from = hex::encode(source);
                if !room::governance_change_allowed(
                    self.room_doc.as_ref(),
                    &candidate,
                    false,
                    Some(&pinned),
                    &from,
                ) {
                    return Err("governance writer is not pinned authority".into());
                }
                if !room::immutable_receipts(self.room_doc.as_ref(), &candidate) {
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
                crate::coverage::monotonic(self.room_doc.as_ref(), &candidate)?;
                let changed = self
                    .room_doc
                    .as_ref()
                    .is_none_or(|doc| doc.base_heads() != candidate.base_heads());
                self.receipts = receipts;
                self.room_doc = Some(candidate);
                let pending = std::mem::take(&mut self.pending);
                self.pending_bytes = 0;
                let mut applied = changed;
                let mut metadata_changed = false;
                let mut rejected = 0;
                for (author, message) in pending {
                    match self.receive(author, message, now) {
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
                    return self.defer(source, message);
                }
                self.receive_document(source, message, now)
            }
            _ => Ok(ReceiveOutcome::default()),
        }
    }
    fn defer(&mut self, source: [u8; 32], message: NeighbourMsg) -> Result<ReceiveOutcome, String> {
        let bytes = postcard::to_allocvec(&message).map_err(err)?;
        if self
            .pending
            .iter()
            .any(|(key, prior)| key == &source && *prior == message)
        {
            return Ok(ReceiveOutcome {
                deferred: true,
                ..Default::default()
            });
        }
        if self.pending.len() >= 64 || bytes.len() > 4 * 1024 * 1024usize - self.pending_bytes {
            return Err("app prerequisite queue full; fresh checkpoint required".into());
        }
        self.pending_bytes += bytes.len();
        self.pending.push_back((source, message));
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
        let role = self.role(&source, now)?;
        let cert = self.certificate(&source, now)?;
        let (_, _, caps) =
            room::extract_governed_fields(self.room_doc.as_ref().ok_or("missing governance")?);
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
            Some(now_secs(now)),
        ) != CapDecision::Allow
        {
            return Err("document capability forbidden".into());
        }
        let payload = collab_types::validate_document_payload_route(
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
        let mut candidate = self
            .documents
            .get(&route)
            .cloned()
            .unwrap_or_else(|| Document {
                doc: AutoCommit::new(),
                channel: ChannelState::default(),
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
            now,
            Duration::from_secs(300),
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
        let mutation=match payload {
            collab_types::RoutedDocumentPayload::Cleartext(mutation) if !candidate.encryption_required=>mutation,
            collab_types::RoutedDocumentPayload::Encrypted{..}=>{
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
        if let DocumentMutation::Snapshot(bytes) = &mutation {
            let entries = crate::coverage::entries(self.room_doc.as_ref().ok_or("missing room")?)?;
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
            candidate.covered_snapshot = Some(bytes.clone());
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
        } else if !self.documents.contains_key(&route) {
            return self.defer(
                source,
                NeighbourMsg::AppDocument {
                    peer,
                    app_id: route.app,
                    cluster_id,
                    record_id: route.record,
                    envelope,
                },
            );
        }
        let changed = document::merge_committed(&mut candidate.doc, &mutation)
            .map_err(|e| format!("{e:?}"))?;
        if matches!(mutation, DocumentMutation::Snapshot(_)) {
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
        let seq = self
            .documents
            .get(&route)
            .and_then(|entry| entry.channel.last_seq_per_author.get(&author))
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("sequence exhausted")?;
        let mut op = SignedOp::new_unsigned(
            mutation.encode().map_err(err)?,
            collab_types::document_artifact_id(app, &self.room, record),
            author,
            collab_types::document_attestation_id(app, &self.room, record),
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
                self.room_doc.as_mut().ok_or("missing room governance")?,
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
                encryption_required: entry.encryption_required,
                covered: entry.covered.clone(),
                covered_snapshot: entry.covered_snapshot.clone(),
            })
            .collect();
        postcard::to_allocvec(&Durable {
            version: 2,
            room: self.room.clone(),
            authority: self.authority.clone(),
            room_bytes: self.room_doc.clone().map(|mut doc| doc.save()),
            documents,
            receipts: self.receipts.clone(),
            certificates: self.certificates.values().cloned().collect(),
            revocations: {
                let mut entries = self.revocations.iter().cloned().collect::<Vec<_>>();
                entries.sort_by_key(|entry| entry.revoked_device_public_key);
                entries
            },
        })
        .map_err(err)
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
        if version != 2 {
            return Err("incomplete: legacy admission lacks durable identity/revocation prerequisites; preserve bytes for authenticated migration".into());
        }
        let durable: Durable = postcard::from_bytes(bytes).map_err(err)?;
        if durable.version != 2 || durable.room != room || durable.authority != authority {
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
            .map(|bytes| AutoCommit::load(&bytes).map_err(err))
            .transpose()?;
        for document in durable.documents {
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
                    encryption_required: document.encryption_required,
                    covered: document.covered,
                    covered_snapshot: document.covered_snapshot,
                },
            );
        }
        Ok(peer)
    }
}
#[derive(Serialize, Deserialize)]
struct GovernedIdentity {
    device_cert: DeviceCert,
    #[serde(default)]
    revocations: Vec<qos_user_identity::RevocationEntry>,
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
