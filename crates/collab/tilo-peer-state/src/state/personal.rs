//! One person's own documents: the store behind `(app, personal)` records.
//!
//! Every device of the person keeps its own copy, opened on the profile's
//! identity root. A signed write is admitted when the device that made it
//! holds a certificate the root signed, the certificate is current and the
//! device is not revoked; everything else is refused. Nothing is joined and
//! nothing is governed: the root's signature says who may write, and a
//! signed revocation says who may not.
//!
//! The documents are the engine's own — [`CachedDocument`] and
//! [`ChannelState`] — admitted through the same payload validation, op
//! validation, appearance check and merge a document received for a
//! workspace goes through. An encrypted write has no place here: the key of
//! the store is the identity itself, and its documents travel only between
//! devices that already share it.

use super::{
    DocumentRow, PeerDocument, ReceiveOutcome, Route, backend, default_surfaces, err, now_ns,
    peer_handle,
};
use crate::NeighbourMsg;
use crate::admission::{DocumentAdmission, ExactAdmissions};
use crate::cached_document::CachedDocument;
use crate::verified_certificate::VerifiedCertificate;
use automerge::AutoCommit;
use automerge::transaction::Transactable as _;
use collab_types::{
    ArtifactKind, ChannelState, DocumentMutation, Ed25519PublicKey, OpKind, SignedOp,
    WholeDocumentPayload,
};
use ed25519_dalek::SigningKey;
use neighbour_wire::{RefusalReason, WriteRefusal};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use tilo_action_policy::{Authority, Party, TreePath};
use tilo_lock::MutexRecover;
use tilo_user_identity::{DeviceCert, RevocationEntry, RevocationList};
use web_time::{Duration, SystemTime};

#[cfg(not(target_arch = "wasm32"))]
pub mod link;

/// The version of [`PersonalStore::durable`] this build writes, and the one
/// it still reads: a store kept before refusals were held there opens with
/// none.
const STORE_VERSION: u32 = 2;
/// The version it held before, which this build still opens.
const STORE_VERSION_PLAIN: u32 = 1;

/// A write's clock may lead or trail the receiving device by this much.
const CLOCK_SKEW: Duration = Duration::from_secs(300);

/// One person's own documents, held by one of their devices.
#[derive(Clone)]
pub struct PersonalStore {
    /// The identity root every writing device's certificate is signed by.
    root: [u8; 32],
    /// This device: the one whose key signs the writes it publishes.
    device: DeviceCert,
    /// The devices admitted to write, by their device key.
    certificates: BTreeMap<[u8; 32], VerifiedCertificate>,
    /// The devices their identity root has revoked.
    revocations: RevocationList,
    documents: BTreeMap<Route, PeerDocument>,
    /// Which writes were decided already, so a replayed frame changes nothing.
    exact: ExactAdmissions,
    /// Every refusal this device holds: its own verdicts on its siblings'
    /// writes, and its siblings' verdicts on its own ([`WriteRefusal`]),
    /// signed by the device that made each and kept on to the next link. A
    /// refusal is one device's, never the store's.
    refusals: Vec<WriteRefusal>,
    /// The authors this device has given its verdicts to, so a set that
    /// became empty is sent as empty rather than read as no news.
    told: BTreeSet<[u8; 32]>,
    /// Whether what this device refuses moved since it was last asked: the
    /// links send the sets it names.
    refusals_moved: bool,
}

impl PersonalStore {
    /// Open the store on the identity `root`, held by `device`.
    pub fn open(root: [u8; 32], device: DeviceCert, now: SystemTime) -> Result<Self, String> {
        let verified = admitted(root, &device, now)?;
        let mut certificates = BTreeMap::new();
        certificates.insert(device.device_public_key, verified);
        Ok(Self {
            root,
            device,
            certificates,
            revocations: RevocationList::new(),
            documents: BTreeMap::new(),
            exact: ExactAdmissions::default(),
            refusals: Vec::new(),
            told: BTreeSet::new(),
            refusals_moved: false,
        })
    }

    /// The identity root the store opens on.
    #[must_use]
    pub fn root(&self) -> [u8; 32] {
        self.root
    }

    /// The store's key in the routing key's text form: the cluster every
    /// record of this person's own is routed under.
    ///
    /// It is derived from the identity root alone, so two devices of one
    /// person agree on where a document lives without either naming the
    /// other, and an app's own route for one of its records names the same
    /// cluster as the store that carries it.
    #[must_use]
    pub fn store_key(&self) -> String {
        collab_types::personal_cluster(&self.root).to_string()
    }

    /// This device's certificate.
    #[must_use]
    pub fn device(&self) -> &DeviceCert {
        &self.device
    }

    /// The devices admitted to write, this one first.
    #[must_use]
    pub fn devices(&self) -> Vec<DeviceCert> {
        self.certificates
            .values()
            .map(|certificate| certificate.as_ref().clone())
            .collect()
    }

    /// The revocations the identity root signed.
    #[must_use]
    pub fn revocations(&self) -> Vec<RevocationEntry> {
        sorted(self.revocations.iter().cloned().collect())
    }

    /// Every refusal this device holds: its own verdicts on its siblings'
    /// writes, and theirs on its own. What the person's history and notices
    /// read, and what the next link hands on.
    #[must_use]
    pub fn write_refusals(&self) -> Vec<WriteRefusal> {
        self.refusals.clone()
    }

    /// This device's own verdicts: what it sends its siblings, and no one
    /// else's.
    fn own_refusals(&self) -> Vec<WriteRefusal> {
        self.refusals
            .iter()
            .filter(|refusal| refusal.by == self.device.device_public_key)
            .cloned()
            .collect()
    }

    /// The frame carrying this device's verdicts on `author`'s writes, or
    /// `None` when it holds none: one message for the whole set, so a set
    /// that no longer names a write clears it.
    fn refusals_frame(&self, author: &[u8; 32]) -> Option<NeighbourMsg> {
        if !self.told.contains(author) {
            return None;
        }
        Some(NeighbourMsg::WriteRefusals {
            refusals: self.refusals_about(author),
        })
    }

    /// This device's verdicts on `author`'s writes, as it holds them now.
    fn refusals_about(&self, author: &[u8; 32]) -> Vec<WriteRefusal> {
        self.own_refusals()
            .into_iter()
            .filter(|refusal| &refusal.author == author)
            .collect()
    }

    /// The sets this device's verdicts moved into since it was last asked,
    /// one frame each: an author it no longer refuses anything of is told so
    /// with an empty set.
    pub fn take_refusal_updates(&mut self) -> Vec<NeighbourMsg> {
        if !self.refusals_moved {
            return Vec::new();
        }
        self.refusals_moved = false;
        self.told
            .iter()
            .map(|author| NeighbourMsg::WriteRefusals {
                refusals: self.refusals_about(author),
            })
            .collect()
    }

    /// File this device's refusal of `envelope`'s write, signed by `key`, the
    /// device's own. What the person who wrote it is shown, and what its
    /// history joins to.
    fn file_refusal(
        &mut self,
        app: &str,
        record: &str,
        envelope: &SignedOp,
        reason: &str,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        if key.verifying_key().to_bytes() != self.device.device_public_key {
            return Err("a refusal is signed by the refusing device's own key".into());
        }
        // A device the person revoked refuses nothing: its verdicts are not
        // the store's to keep, and nothing of theirs is sent on.
        if self.revoked_here() {
            return Ok(());
        }
        let author = *envelope.author.as_bytes();
        // A device the person revoked is told once why its writes stop, not
        // once per write: what it already learned of its own revocation is
        // not repeated by every write it sends.
        if self.revocations.is_revoked(&author) {
            self.refusals.retain(|held| {
                !(held.by == self.device.device_public_key && held.author == author)
            });
        }
        let mut refusal = WriteRefusal {
            workspace: self.store_key(),
            app: app.into(),
            record: record.into(),
            author,
            seq: envelope.seq,
            operation: format!("{}:{}", envelope.artifact.0, envelope.seq),
            changes: self.unheld_changes(app, record, envelope),
            reason: RefusalReason::of(reason),
            detail: reason.into(),
            by: self.device.device_public_key,
            at_ns: now_ns(now),
            signature: Vec::new(),
        };
        refusal.sign(key)?;
        // One verdict per write: a later reason for the same write replaces
        // the earlier, and a write whose later sequence was admitted drops
        // every refusal at or below it on the same route.
        self.refusals.retain(|held| {
            !(held.by == refusal.by
                && held.author == refusal.author
                && held.app == refusal.app
                && held.record == refusal.record
                && held.seq == refusal.seq)
        });
        self.refusals.push(refusal);
        if author != self.device.device_public_key {
            self.told.insert(author);
        }
        self.refusals_moved = true;
        Ok(())
    }

    /// The changes of `envelope`'s write this store does not hold, as their
    /// hashes: what the author's history joins a refusal to. A whole-document
    /// snapshot names the writes it brings, never the ones already here.
    fn unheld_changes(&self, app: &str, record: &str, envelope: &SignedOp) -> Vec<[u8; 32]> {
        let mut changes = change_hashes(&envelope.payload);
        let Some(held) = self.documents.get(&Self::route(app, record)) else {
            return changes;
        };
        // A copy shares the document until asked to change it, so reading every
        // change it holds costs a pointer copy.
        let mut held = held.doc.clone();
        let held: BTreeSet<[u8; 32]> = held
            .get_changes(&[])
            .iter()
            .map(|change| change.hash().0)
            .collect();
        changes.retain(|hash| !held.contains(hash));
        changes
    }

    /// Take `source`'s verdicts on this device's writes: each carries its
    /// signature over exactly its contents, from a device this store holds a
    /// live certificate for. The whole set replaces that device's earlier
    /// one, so what it no longer refuses is cleared here too.
    fn receive_refusals(
        &mut self,
        source: [u8; 32],
        refusals: Vec<WriteRefusal>,
        now: SystemTime,
    ) -> Result<ReceiveOutcome, String> {
        if refusals.iter().any(|refusal| refusal.by != source) {
            return Err("a refusal is signed by the device that sent it".into());
        }
        if !refusals.is_empty() {
            self.admits_device(&source, now)?;
            if !self.certificates.contains_key(&source) {
                return Err("the device's certificate is not here".into());
            }
            if refusals.iter().any(|refusal| !refusal.signed_by(&source)) {
                return Err("a refusal is not the refusing device's signature".into());
            }
        }
        // Only a verdict on this device's own writes is this device's to
        // keep: what the refuser said of a sibling reaches that sibling.
        let mine = self.device.device_public_key;
        let received: Vec<WriteRefusal> = refusals
            .into_iter()
            .filter(|refusal| refusal.author == mine)
            .collect();
        let changed = {
            let held: Vec<WriteRefusal> = self
                .refusals
                .iter()
                .filter(|refusal| refusal.by == source)
                .cloned()
                .collect();
            held != received
        };
        self.refusals.retain(|refusal| refusal.by != source);
        self.refusals.extend(received);
        // Received verdicts are not echoed: each device sends its own.
        self.refusals.sort_by_key(|refusal| refusal.by);
        Ok(ReceiveOutcome {
            metadata_changed: changed,
            ..Default::default()
        })
    }

    /// Admit `certificate` to write: a certificate this store's root signed,
    /// current, and not a store node's. Returns whether it was new here.
    pub fn admit_device(
        &mut self,
        certificate: &DeviceCert,
        now: SystemTime,
    ) -> Result<bool, String> {
        if self.revocations.refuses(certificate) {
            return Err("device revoked".into());
        }
        let verified = admitted(self.root, certificate, now)?;
        Ok(self
            .certificates
            .insert(certificate.device_public_key, verified)
            .is_none())
    }

    /// Hold a revocation the identity root signed. Returns whether it
    /// changed the store: an earlier revocation of the same device stands.
    ///
    /// A revocation of this device ends its hold on the documents: what the
    /// person took away from it, it keeps none of.
    pub fn revoke(&mut self, entry: RevocationEntry) -> Result<bool, String> {
        if entry.user_public_key != self.root {
            return Err("the revocation names another identity".into());
        }
        let revoked = entry.revoked_device_public_key;
        let changed = self.revocations.insert(entry).map_err(err)?;
        if changed {
            // What the person took from a device is no longer refused by
            // anyone: the verdicts about it are forgotten with it.
            let before = self.refusals.len();
            self.refusals.retain(|refusal| refusal.author != revoked);
            self.refusals_moved |= self.refusals.len() != before;
        }
        if self.revoked_here() {
            self.forget_documents();
        }
        Ok(changed)
    }

    /// Whether the identity root revoked this device: it is no longer one of
    /// the person's devices, and holds none of their documents.
    fn revoked_here(&self) -> bool {
        self.revocations.refuses(&self.device)
    }

    /// Let go of every document and every decided write: a device the person
    /// revoked reads and writes nothing of theirs from here on.
    fn forget_documents(&mut self) {
        self.documents.clear();
        self.exact = ExactAdmissions::default();
        // A device the person revoked keeps no verdicts of theirs either: not
        // its own on its siblings' writes, nor what they said of its own.
        self.refusals.clear();
        self.refusals_moved = true;
    }

    /// Whether `device` may hold a link to this store: it is not revoked, and
    /// a certificate already held for it is still current. A device never met
    /// is admitted to the connection alone; nothing it sends is admitted
    /// before the certificate its first frame carries checks out.
    pub fn admits_device(&self, device: &[u8; 32], now: SystemTime) -> Result<(), String> {
        if self.revocations.is_revoked(device) {
            return Err("device revoked".into());
        }
        if let Some(certificate) = self.certificates.get(device) {
            certificate.current_typed(now_ns(now)).map_err(err)?;
        }
        Ok(())
    }

    /// What this device says first: its certificate, and the revocations its
    /// identity signed.
    #[must_use]
    pub fn hello(&self) -> NeighbourMsg {
        NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(self.device.clone()),
            revocations: self.revocations(),
        }
    }

    /// Whether this store holds `record` of `app`.
    #[must_use]
    pub fn holds_document(&self, app: &str, record: &str) -> bool {
        self.documents.contains_key(&Self::route(app, record))
    }

    /// The whole document, as a device receiving it would save it.
    #[must_use]
    pub fn document(&self, app: &str, record: &str) -> Option<Vec<u8>> {
        self.documents
            .get(&Self::route(app, record))
            .map(|held| held.doc.saved())
    }

    /// Every document held, by what it belongs to.
    #[must_use]
    pub fn rows(&self) -> Vec<DocumentRow> {
        self.documents
            .iter()
            .map(|(route, held)| DocumentRow {
                app_id: route.app.clone(),
                record_id: route.record.clone(),
                heads: held
                    .doc
                    .base_heads()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            })
            .collect()
    }

    fn route(app: &str, record: &str) -> Route {
        Route {
            app: app.into(),
            record: record.into(),
        }
    }

    /// Sign `mutation` of `record` with this device's key, admit it, and
    /// return the frame to publish to the person's other devices.
    pub fn publish(
        &mut self,
        app: &str,
        record: &str,
        mutation: &DocumentMutation,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        let author = Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
        let signed = self.sign(app, record, mutation, &author, key, now)?;
        self.admit_local(app, record, signed.clone(), key, now)?;
        Ok(self.frame(app, record, signed))
    }

    /// Admit a write this device signed itself: what an app publishing one of
    /// its own records does, as `admit_local_app_document` does in a
    /// workspace. Returns the frame to publish to the person's other devices.
    pub fn admit_local(
        &mut self,
        app: &str,
        record: &str,
        envelope: SignedOp,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        if envelope.author.as_bytes() != &self.device.device_public_key {
            return Err("this device signs its own writes".into());
        }
        let mut candidate = self.clone();
        let refused = candidate.write(app, record, &envelope, now, true).err();
        *self = candidate;
        if let Some(reason) = refused {
            // The store's verdict on the person's own write is filed like a
            // sibling's: what its history shows, and what a notice is made of.
            self.file_refusal(app, record, &envelope, &reason, key, now)?;
            return Err(reason);
        }
        Ok(self.frame(app, record, envelope))
    }

    /// What another device may lack: this device's certificate and the
    /// revocations its identity signed, then a signed snapshot of every
    /// document held. Whoever holds a document can hand over its whole state,
    /// so a device added later catches up from any of its siblings.
    pub fn catch_up(
        &mut self,
        key: &SigningKey,
        remote: [u8; 32],
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let mut messages = vec![self.hello()];
        // What this device refuses of the peer's writes reaches it on the
        // link, live or here, even when it is revoked and gets nothing else:
        // a person whose device was taken away is told why their writes stop.
        if let Some(frame) = self.refusals_frame(&remote) {
            messages.push(frame);
        }
        // A revoked device says only who it is and what was refused of
        // theirs: its siblings keep no documents from it.
        if self.revoked_here() {
            return Ok(messages);
        }
        let routes: Vec<Route> = if self.admits_device(&remote, now).is_ok() {
            self.documents.keys().cloned().collect()
        } else {
            Vec::new()
        };
        for route in routes {
            let Some(held) = self.documents.get(&route) else {
                continue;
            };
            let whole = held.doc.saved();
            messages.push(self.publish(
                &route.app,
                &route.record,
                &DocumentMutation::Snapshot(whole),
                key,
                now,
            )?);
        }
        Ok(messages)
    }

    /// Admit a frame `source` sent, `source` being the device key its
    /// connection proved.
    pub fn receive(
        &mut self,
        key: &SigningKey,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<ReceiveOutcome, String> {
        if key.verifying_key().to_bytes() != self.device.device_public_key {
            return Err("a store reads with the key of the device that holds it".into());
        }
        let mut candidate = self.clone();
        let outcome = candidate.receive_inner(key, source, message, now);
        // A refused write leaves the store as it was, and its refusal is kept:
        // the candidate carries both, so it is taken either way.
        *self = candidate;
        outcome
    }

    fn receive_inner(
        &mut self,
        key: &SigningKey,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<ReceiveOutcome, String> {
        match message {
            NeighbourMsg::IdentityHello {
                protocol,
                cert,
                revocations,
            } => {
                // A device at another peer protocol version is admitted to
                // nothing: what it says is not read at this one.
                if protocol != neighbour_wire::PEER_PROTOCOL_VERSION {
                    return Err(format!(
                        "the device speaks peer protocol {protocol}, this one {}",
                        neighbour_wire::PEER_PROTOCOL_VERSION
                    ));
                }
                self.receive_hello(source, &cert, revocations, now)
            }
            message @ NeighbourMsg::AppDocument { .. } => {
                self.receive_document(key, source, message, now)
            }
            NeighbourMsg::WriteRefusals { refusals } => {
                self.receive_refusals(source, refusals, now)
            }
            _ => Err("the personal store reads no other message".into()),
        }
    }

    /// Admit the transport device's own certificate and the revocations its
    /// identity root signed.
    fn receive_hello(
        &mut self,
        source: [u8; 32],
        cert: &DeviceCert,
        revocations: Vec<RevocationEntry>,
        now: SystemTime,
    ) -> Result<ReceiveOutcome, String> {
        if cert.device_public_key != source {
            return Err("transport certificate mismatch".into());
        }
        let mut metadata_changed = false;
        // A device the person revoked is admitted to nothing it says, and
        // told what it was refused instead of being dropped at the door: a
        // person whose device was taken away learns why their writes stop.
        if !self.revocations.is_revoked(&source) {
            let held = self
                .certificates
                .get(&source)
                .map(AsRef::as_ref)
                .is_some_and(|known| known == cert);
            if !held {
                metadata_changed = self.admit_device(cert, now)?;
            }
        }
        for entry in revocations {
            metadata_changed |= self.revoke(entry)?;
        }
        Ok(ReceiveOutcome {
            metadata_changed,
            ..Default::default()
        })
    }

    fn receive_document(
        &mut self,
        key: &SigningKey,
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
        let refused = if cluster_id != self.store_key()
            || peer != peer_handle(&source)
            || envelope.author.as_bytes() != &source
        {
            Some("transport author or store key mismatch".to_owned())
        } else {
            match self.write(&app_id, &record_id, &envelope, now, false) {
                Ok(outcome) => return Ok(outcome),
                Err(reason) => Some(reason),
            }
        };
        let reason = refused.unwrap_or_default();
        // The store's verdict on this one write is now a fact it keeps: the
        // write's author is told, and its history joins it to the write.
        self.file_refusal(&app_id, &record_id, &envelope, &reason, key, now)?;
        Err(reason)
    }

    /// The one admission: the signed write is the author's own, its device is
    /// certified by the root and live, its route is this store, and what it
    /// changes merges into the record. Everything is decided in a copy, so a
    /// refusal leaves the store as it was.
    fn write(
        &mut self,
        app: &str,
        record: &str,
        envelope: &SignedOp,
        now: SystemTime,
        local: bool,
    ) -> Result<ReceiveOutcome, String> {
        let source = *envelope.author.as_bytes();
        if self.revoked_here() {
            return Err("this device was revoked".into());
        }
        if local {
            if source != self.device.device_public_key {
                return Err("this device signs its own writes".into());
            }
        } else {
            self.admits_device(&source, now)?;
            if !self.certificates.contains_key(&source) {
                return Err("the device's certificate is not here".into());
            }
        }
        let store_key = self.store_key();
        let admitted =
            DocumentAdmission::from_op(&store_key, app, record, envelope).map_err(err)?;
        if self.exact.get(&admitted.key) == Some(&admitted.operation_id) {
            return Ok(ReceiveOutcome {
                duplicate: true,
                ..Default::default()
            });
        }
        let payload =
            collab_types::validate_whole_document_payload_route(app, &store_key, record, envelope)
                .map_err(err)?;
        let WholeDocumentPayload::Cleartext(mutation) = payload else {
            return Err("a personal document is cleartext between its devices".into());
        };
        if envelope.kind != OpKind::Change {
            return Err("the personal store admits document changes".into());
        }
        let route = Self::route(app, record);
        let route_existed = self.documents.contains_key(&route);
        if !route_existed && matches!(mutation, DocumentMutation::Incremental(_)) {
            return Err("the change arrived before the document it changes".into());
        }
        let mut candidate = self
            .documents
            .get(&route)
            .cloned()
            .unwrap_or_else(|| PeerDocument {
                doc: AutoCommit::new().into(),
                channel: ChannelState::default(),
                encryption_required: false,
                covered: BTreeMap::new(),
                covered_snapshot: None,
                sealed: backend::SealedLog::default(),
            });
        candidate
            .channel
            .artifacts
            .insert(envelope.artifact, ArtifactKind::Note);
        candidate
            .channel
            .artifact_apps
            .insert(envelope.artifact, app.into());
        candidate
            .channel
            .artifact_records
            .insert(envelope.artifact, record.into());
        candidate.channel.room.clone_from(&store_key);
        let mut validated = candidate.channel.clone();
        validated.surfaces = default_surfaces();
        // The person owns every node of their own store: each write is
        // decided as the edit of its record that it is.
        let authority = Authority::person(Party::default().owning(TreePath::root()));
        collab_types::validate_admitted_inbound_op(
            envelope,
            &authority,
            &mut validated,
            now,
            CLOCK_SKEW,
        )
        .map_err(|error| format!("the write is not admitted: {error}"))?;
        candidate.channel = validated;
        collab_types::check_claimed(envelope)?;
        if let DocumentMutation::Incremental(bytes) = &mutation {
            let changes = collab_types::parse_changes(bytes).map_err(err)?;
            collab_types::check_appearances(&changes, None)
                .map_err(|refusal| format!("document change forbidden: {refusal}"))?;
        }
        let changed =
            match crate::document::merge_committed_with_status(&mut candidate.doc, &mutation)
                .map_err(|error| format!("{error:?}"))?
            {
                crate::document::MergeOutcome::Changed(changed) => changed,
                crate::document::MergeOutcome::MissingDependencies => {
                    return Err("the change's earlier writes are not here".into());
                }
            };
        self.exact.insert(&admitted).map_err(err)?;
        self.documents.insert(route, candidate);
        self.landed(&source, app, record, envelope.seq);
        Ok(ReceiveOutcome {
            changed,
            metadata_changed: changed,
            ..Default::default()
        })
    }

    /// The author's whole state is offered on every link, so a write at or
    /// past a refused one on the same route means that one landed too: what
    /// this device refused of this author is cleared.
    fn landed(&mut self, author: &[u8; 32], app: &str, record: &str, seq: u64) {
        let before = self.refusals.len();
        self.refusals.retain(|refusal| {
            !(refusal.by == self.device.device_public_key
                && refusal.author == *author
                && refusal.app == app
                && refusal.record == record
                && refusal.seq <= seq)
        });
        self.refusals_moved |= self.refusals.len() != before;
    }

    fn sign(
        &self,
        app: &str,
        record: &str,
        mutation: &DocumentMutation,
        author: &Ed25519PublicKey,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<SignedOp, String> {
        if author.as_bytes() != &self.device.device_public_key {
            return Err("this device signs its own writes".into());
        }
        let store_key = self.store_key();
        let seq = self
            .documents
            .get(&Self::route(app, record))
            .and_then(|held| held.channel.last_seq_per_author.get(author))
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("sequence exhausted")?;
        let mut op = SignedOp::new_unsigned(
            mutation.encode().map_err(err)?,
            collab_types::document_artifact_id(app, &store_key, record),
            Ed25519PublicKey::from_bytes(*author.as_bytes()),
            collab_types::document_attestation_id(app, &store_key, record),
            OpKind::Change,
            now,
            seq,
        );
        collab_types::sign_op(&mut op, key).map_err(err)?;
        Ok(op)
    }

    fn frame(&self, app: &str, record: &str, envelope: SignedOp) -> NeighbourMsg {
        NeighbourMsg::AppDocument {
            peer: peer_handle(envelope.author.as_bytes()),
            app_id: app.into(),
            cluster_id: self.store_key(),
            record_id: record.into(),
            envelope: Box::new(envelope),
        }
    }

    /// The store as bytes to keep beside the profile's workspaces.
    pub fn durable(&self) -> Result<Vec<u8>, String> {
        let documents = self
            .documents
            .iter()
            .map(|(route, held)| DurableDocument {
                app: route.app.clone(),
                record: route.record.clone(),
                bytes: held.doc.saved(),
                watermarks: held
                    .channel
                    .last_seq_per_author
                    .iter()
                    .map(|(key, seq)| (*key.as_bytes(), *seq))
                    .collect(),
            })
            .collect();
        let durable = Durable {
            version: STORE_VERSION,
            root: self.root,
            device: self.device.clone(),
            certificates: self
                .certificates
                .values()
                .map(|certificate| certificate.as_ref().clone())
                .collect(),
            revocations: self.revocations(),
            documents,
            exact: self.exact.encode().map_err(err)?,
            refusals: self.refusals.clone(),
            told: self.told.iter().copied().collect(),
        };
        postcard::to_allocvec(&durable).map_err(err)
    }

    /// The device that kept the store `bytes` holds, when they are a store
    /// this build reads.
    #[must_use]
    pub fn kept_by(bytes: &[u8]) -> Option<[u8; 32]> {
        // ignored-failure: bytes this build cannot read name no device; `restore` reports why
        postcard::from_bytes::<Durable>(bytes)
            .map(|durable| durable.device.device_public_key)
            .or_else(|_| {
                postcard::from_bytes::<DurablePlain>(bytes)
                    .map(|plain| plain.device.device_public_key)
            })
            .ok()
    }

    /// Open the store `bytes` holds, as `device`, which must be the device
    /// that kept them.
    pub fn restore(bytes: &[u8], device: &DeviceCert, now: SystemTime) -> Result<Self, String> {
        // A store kept before refusals were held beside the revocations is
        // read as one holding none: the person's documents are never lost to
        // a field added later.
        let durable: Durable = match postcard::from_bytes(bytes) {
            Ok(durable) => durable,
            Err(error) => {
                let plain: DurablePlain = postcard::from_bytes(bytes).map_err(err)?;
                if plain.version != STORE_VERSION_PLAIN {
                    return Err(error.to_string());
                }
                Durable {
                    version: STORE_VERSION_PLAIN,
                    root: plain.root,
                    device: plain.device,
                    certificates: plain.certificates,
                    revocations: plain.revocations,
                    documents: plain.documents,
                    exact: plain.exact,
                    refusals: Vec::new(),
                    told: Vec::new(),
                }
            }
        };
        if durable.version != STORE_VERSION && durable.version != STORE_VERSION_PLAIN {
            return Err(format!(
                "the personal store is version {}, this build reads {STORE_VERSION}",
                durable.version
            ));
        }
        if durable.device.device_public_key != device.device_public_key {
            return Err("the personal store belongs to another device".into());
        }
        let mut store = Self::open(durable.root, durable.device, now)?;
        for certificate in durable.certificates {
            store.admit_device(&certificate, now)?;
        }
        for entry in durable.revocations {
            store.revoke(entry)?;
        }
        let store_key = store.store_key();
        for document in durable.documents {
            let route = Self::route(&document.app, &document.record);
            let doc = AutoCommit::load(&document.bytes).map_err(err)?;
            let artifact =
                collab_types::document_artifact_id(&document.app, &store_key, &document.record);
            let mut channel = ChannelState::default();
            channel.artifacts.insert(artifact, ArtifactKind::Note);
            channel.artifact_apps.insert(artifact, document.app);
            channel.artifact_records.insert(artifact, document.record);
            channel.room.clone_from(&store_key);
            channel.surfaces = default_surfaces();
            channel.last_seq_per_author = document
                .watermarks
                .into_iter()
                .map(|(key, seq)| (Ed25519PublicKey::from_bytes(key), seq))
                .collect();
            store.documents.insert(
                route,
                PeerDocument {
                    doc: CachedDocument::from(doc),
                    channel,
                    encryption_required: false,
                    covered: BTreeMap::new(),
                    covered_snapshot: None,
                    sealed: backend::SealedLog::default(),
                },
            );
        }
        store.exact = ExactAdmissions::decode(&durable.exact).map_err(err)?;
        for refusal in durable.refusals {
            // A verdict only the device that made it can stand behind, and
            // only about a write of this device's: anything else is no state
            // of this store's.
            let by_us = refusal.by == store.device.device_public_key;
            let about_us = refusal.author == store.device.device_public_key;
            if (by_us || about_us) && refusal.signed_by(&refusal.by) {
                store.refusals.push(refusal);
            }
        }
        store.told = durable.told.into_iter().collect();
        // A device revoked after it kept these bytes keeps none of them.
        if store.revoked_here() {
            store.forget_documents();
        }
        Ok(store)
    }
}

/// The automerge changes `payload` carries, as their hashes: what the
/// author's history joins a refusal to. A payload that does not parse
/// carries none.
pub(super) fn change_hashes(payload: &[u8]) -> Vec<[u8; 32]> {
    let Ok(mutation) = DocumentMutation::decode(payload) else {
        return Vec::new();
    };
    let bytes = match mutation {
        DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
    };
    // ignored-failure: a payload that is not automerge bytes names no changes this device lacks
    collab_types::parse_changes(&bytes)
        .unwrap_or_default()
        .iter()
        .map(automerge::Change::hash)
        .map(|hash| hash.0)
        .collect()
}

/// A device's certificate as the store admits it: one this store's root
/// signed, current, and not a store node's. A device that keeps other
/// people's data holds none of anyone's own documents.
fn admitted(
    root: [u8; 32],
    certificate: &DeviceCert,
    now: SystemTime,
) -> Result<VerifiedCertificate, String> {
    if certificate.user_public_key != root {
        return Err("the certificate is not this identity's".into());
    }
    let verified = VerifiedCertificate::new(certificate.clone(), now_ns(now))?;
    if verified.capabilities.can_act_as_host {
        return Err("a store node holds no personal documents".into());
    }
    Ok(verified)
}

fn sorted(mut entries: Vec<RevocationEntry>) -> Vec<RevocationEntry> {
    entries.sort_by_key(|entry| entry.revoked_device_public_key);
    entries
}

/// The store as it is kept: rooted in the identity, and free of anything the
/// engine re-derives from a document's route.
#[derive(Serialize, Deserialize)]
struct Durable {
    version: u32,
    root: [u8; 32],
    device: DeviceCert,
    certificates: Vec<DeviceCert>,
    revocations: Vec<RevocationEntry>,
    documents: Vec<DurableDocument>,
    exact: Vec<u8>,
    refusals: Vec<WriteRefusal>,
    told: Vec<[u8; 32]>,
}

/// The store as it was kept before it held refusals: this build reads it so
/// a person's own documents survive the field being added.
#[derive(Serialize, Deserialize)]
struct DurablePlain {
    version: u32,
    root: [u8; 32],
    device: DeviceCert,
    certificates: Vec<DeviceCert>,
    revocations: Vec<RevocationEntry>,
    documents: Vec<DurableDocument>,
    exact: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct DurableDocument {
    app: String,
    record: String,
    bytes: Vec<u8>,
    watermarks: Vec<([u8; 32], u64)>,
}

/// A device's personal store, shared by whatever reads and writes it.
///
/// Clones are the same store: an app's write, the runtime's durable copy and
/// the links to the person's other devices all see one state.
#[derive(Clone)]
pub struct PeerPersonal(Arc<Mutex<PersonalStore>>);

impl PeerPersonal {
    /// Hold `store`.
    #[must_use]
    pub fn new(store: PersonalStore) -> Self {
        Self(Arc::new(Mutex::new(store)))
    }

    /// Read the store.
    pub fn read<R>(&self, read: impl FnOnce(&PersonalStore) -> R) -> R {
        read(&self.0.lock_recover())
    }

    /// Change the store.
    pub fn write<R>(&self, write: impl FnOnce(&mut PersonalStore) -> R) -> R {
        write(&mut self.0.lock_recover())
    }

    /// The store's key in the routing key's text form.
    #[must_use]
    pub fn store_key(&self) -> String {
        self.read(PersonalStore::store_key)
    }

    /// Whether this device holds `record` of `app`.
    #[must_use]
    pub fn holds_document(&self, app: &str, record: &str) -> bool {
        self.read(|store| store.holds_document(app, record))
    }

    /// The whole document, as a device receiving it would save it.
    #[must_use]
    pub fn document(&self, app: &str, record: &str) -> Option<Vec<u8>> {
        self.read(|store| store.document(app, record))
    }

    /// Every document held, by what it belongs to.
    #[must_use]
    pub fn rows(&self) -> Vec<DocumentRow> {
        self.read(PersonalStore::rows)
    }

    /// Admit a write this device signed itself, and return the frame to
    /// publish to the person's other devices.
    pub fn admit_local(
        &self,
        app: &str,
        record: &str,
        envelope: SignedOp,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        self.write(|store| store.admit_local(app, record, envelope, key, now))
    }

    /// Every refusal this device holds: its own and its siblings' on its own
    /// writes.
    #[must_use]
    pub fn write_refusals(&self) -> Vec<WriteRefusal> {
        self.read(PersonalStore::write_refusals)
    }

    /// The store as bytes to keep beside the profile's workspaces.
    pub fn durable(&self) -> Result<Vec<u8>, String> {
        self.read(PersonalStore::durable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ReadDoc as _;
    use collab_types::CommitNamed as _;
    use tilo_user_identity::{DeviceCapabilities, RevocationReason, sign_device_cert};

    fn device(
        root: &SigningKey,
        n: u8,
        capabilities: DeviceCapabilities,
    ) -> (SigningKey, DeviceCert) {
        let key = SigningKey::from_bytes(&[n; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: format!("device-{n}"),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, root).unwrap();
        (key, cert)
    }

    fn now() -> SystemTime {
        SystemTime::now()
    }

    /// A record with `field` set to `value`, as an app's first write of it.
    fn record(field: &str, value: &str) -> DocumentMutation {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, field, value).unwrap();
        doc.commit_edit("tube", field);
        DocumentMutation::Snapshot(doc.save())
    }

    /// Changes that depend on no other write of the record.
    fn change(field: &str, value: &str) -> DocumentMutation {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, field, value).unwrap();
        doc.commit_edit("tube", field);
        DocumentMutation::Incremental(doc.save())
    }

    fn field(doc: &[u8], name: &str) -> Option<String> {
        let doc = AutoCommit::load(doc).unwrap();
        let (value, _) = doc.get(automerge::ROOT, name).unwrap()?;
        value.into_string().ok()
    }

    fn revision(doc: &[u8]) -> Vec<String> {
        AutoCommit::load(doc)
            .unwrap()
            .get_heads()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    fn hello(cert: &DeviceCert, revocations: Vec<RevocationEntry>) -> NeighbourMsg {
        NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(cert.clone()),
            revocations,
        }
    }

    fn revocation(root: &SigningKey, device: &SigningKey) -> RevocationEntry {
        let mut entry = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: device.verifying_key().to_bytes(),
            revoked_at_ns: 1,
            reason: RevocationReason::Compromised,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, root).unwrap();
        entry
    }

    #[test]
    fn a_record_written_on_one_device_is_read_on_another_of_the_same_identity() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let root_key = root.verifying_key().to_bytes();
        let (a_key, a_cert) = device(&root, 2, DeviceCapabilities::member());
        let (b_key, b_cert) = device(&root, 3, DeviceCapabilities::member());
        let mut a = PersonalStore::open(root_key, a_cert, now()).unwrap();
        let mut b = PersonalStore::open(root_key, b_cert, now()).unwrap();

        a.publish("tube", "prefs", &record("theme", "dark"), &a_key, now())
            .unwrap();
        // B holds nothing until it meets A, then catches up whole.
        assert!(!b.holds_document("tube", "prefs"));
        for message in a
            .catch_up(&a_key, b_key.verifying_key().to_bytes(), now())
            .unwrap()
        {
            b.receive(&b_key, a_key.verifying_key().to_bytes(), message, now())
                .unwrap();
        }
        let read = b.document("tube", "prefs").unwrap();
        assert_eq!(field(&read, "theme").as_deref(), Some("dark"));
        assert_eq!(
            revision(&read),
            revision(&a.document("tube", "prefs").unwrap())
        );

        // B says who it is, then its own write reaches A, and both hold one
        // document.
        a.receive(&a_key, b_key.verifying_key().to_bytes(), b.hello(), now())
            .unwrap();
        let published = b
            .publish("tube", "prefs", &change("accent", "blue"), &b_key, now())
            .unwrap();
        let outcome = a
            .receive(&a_key, b_key.verifying_key().to_bytes(), published, now())
            .unwrap();
        assert!(outcome.changed);
        assert_eq!(
            field(&a.document("tube", "prefs").unwrap(), "accent").as_deref(),
            Some("blue")
        );
        assert_eq!(
            field(&a.document("tube", "prefs").unwrap(), "theme").as_deref(),
            Some("dark")
        );
        assert_eq!(a.rows(), b.rows());
        assert_eq!(a.rows().len(), 1);
    }

    #[test]
    fn a_device_of_another_identity_writes_nothing_and_holds_nothing() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let other = SigningKey::from_bytes(&[9; 32]);
        let (a_key, a_cert) = device(&root, 2, DeviceCapabilities::member());
        let (other_key, other_cert) = device(&other, 10, DeviceCapabilities::member());
        let mut a = PersonalStore::open(root.verifying_key().to_bytes(), a_cert, now()).unwrap();
        let mut stranger =
            PersonalStore::open(other.verifying_key().to_bytes(), other_cert.clone(), now())
                .unwrap();
        a.publish("tube", "prefs", &record("theme", "dark"), &a_key, now())
            .unwrap();

        // Its certificate is not this identity's: A refuses it outright.
        let refused = a.receive(
            &a_key,
            other_key.verifying_key().to_bytes(),
            hello(&other_cert, Vec::new()),
            now(),
        );
        assert!(refused.is_err());
        assert!(
            !a.devices()
                .iter()
                .any(|cert| cert.device_public_key == other_key.verifying_key().to_bytes())
        );

        // And given A's frames, it admits none of them.
        for message in a
            .catch_up(&a_key, other_key.verifying_key().to_bytes(), now())
            .unwrap()
        {
            assert!(
                stranger
                    .receive(&other_key, a_key.verifying_key().to_bytes(), message, now())
                    .is_err()
            );
        }
        assert!(stranger.rows().is_empty());
        assert!(!stranger.holds_document("tube", "prefs"));
        assert!(
            !stranger
                .devices()
                .iter()
                .any(|cert| cert.device_public_key == a_key.verifying_key().to_bytes())
        );
    }

    #[test]
    fn a_store_node_is_admitted_to_none_of_the_persons_documents() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let root_key = root.verifying_key().to_bytes();
        let (a_key, a_cert) = device(&root, 2, DeviceCapabilities::member());
        let (node_key, node_cert) = device(&root, 4, DeviceCapabilities::host());
        let mut a = PersonalStore::open(root_key, a_cert, now()).unwrap();
        a.publish("tube", "prefs", &record("theme", "dark"), &a_key, now())
            .unwrap();
        // It cannot open a store at all: a store node keeps no personal data.
        assert!(PersonalStore::open(root_key, node_cert.clone(), now()).is_err());
        // And it writes nothing to another device's store.
        assert!(
            a.receive(
                &a_key,
                node_key.verifying_key().to_bytes(),
                hello(&node_cert, Vec::new()),
                now(),
            )
            .is_err()
        );
        assert!(
            !a.devices()
                .iter()
                .any(|cert| cert.device_public_key == node_key.verifying_key().to_bytes())
        );
    }

    #[test]
    fn a_revoked_device_writes_nothing_after_its_revocation() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let (a_key, a_cert) = device(&root, 2, DeviceCapabilities::member());
        let (b_key, b_cert) = device(&root, 3, DeviceCapabilities::member());
        let (c_key, c_cert) = device(&root, 5, DeviceCapabilities::member());
        let mut a = PersonalStore::open(root.verifying_key().to_bytes(), a_cert, now()).unwrap();
        let mut b = PersonalStore::open(root.verifying_key().to_bytes(), b_cert, now()).unwrap();
        a.publish("tube", "prefs", &record("theme", "dark"), &a_key, now())
            .unwrap();
        for message in a
            .catch_up(&a_key, b_key.verifying_key().to_bytes(), now())
            .unwrap()
        {
            b.receive(&b_key, a_key.verifying_key().to_bytes(), message, now())
                .unwrap();
        }
        // B introduces itself, and a write of its is admitted before the
        // revocation.
        a.receive(&a_key, b_key.verifying_key().to_bytes(), b.hello(), now())
            .unwrap();
        let published = b
            .publish("tube", "prefs", &change("accent", "blue"), &b_key, now())
            .unwrap();
        a.receive(&a_key, b_key.verifying_key().to_bytes(), published, now())
            .unwrap();
        assert_eq!(
            field(&a.document("tube", "prefs").unwrap(), "accent").as_deref(),
            Some("blue")
        );

        // A sibling device hands over the revocation their identity signed.
        let outcome = a
            .receive(
                &a_key,
                c_key.verifying_key().to_bytes(),
                hello(&c_cert, vec![revocation(&root, &b_key)]),
                now(),
            )
            .unwrap();
        assert!(outcome.metadata_changed);
        assert!(
            a.revocations()
                .iter()
                .any(|held| held.revoked_device_public_key == b_key.verifying_key().to_bytes())
        );

        // Everything B says after that is refused, and nothing changes.
        let published = b
            .publish("tube", "prefs", &change("font", "serif"), &b_key, now())
            .unwrap();
        assert!(
            a.receive(&a_key, b_key.verifying_key().to_bytes(), published, now())
                .is_err()
        );
        assert_eq!(field(&a.document("tube", "prefs").unwrap(), "font"), None);
        // A hello from B itself admits nothing of it either, and no longer
        // ends the link: a device the person revoked is told what was
        // refused of its writes.
        let heard = a
            .receive(
                &a_key,
                b_key.verifying_key().to_bytes(),
                hello(&b_cert_of(&root, &b_key), Vec::new()),
                now(),
            )
            .unwrap();
        assert!(!heard.changed && !heard.metadata_changed);
    }

    #[test]
    fn a_revoked_device_lets_go_of_the_documents_it_held() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let root_key = root.verifying_key().to_bytes();
        let (a_key, a_cert) = device(&root, 2, DeviceCapabilities::member());
        let (b_key, b_cert) = device(&root, 3, DeviceCapabilities::member());
        let mut a = PersonalStore::open(root_key, a_cert, now()).unwrap();
        let mut b = PersonalStore::open(root_key, b_cert.clone(), now()).unwrap();
        a.publish("tube", "prefs", &record("theme", "dark"), &a_key, now())
            .unwrap();
        for message in a
            .catch_up(&a_key, b_key.verifying_key().to_bytes(), now())
            .unwrap()
        {
            b.receive(&b_key, a_key.verifying_key().to_bytes(), message, now())
                .unwrap();
        }
        assert!(b.holds_document("tube", "prefs"));

        // The person's root revokes B: it holds none of their documents from
        // here on, writes none of them, and offers none.
        assert!(b.revoke(revocation(&root, &b_key)).unwrap());
        assert!(!b.holds_document("tube", "prefs"));
        assert_eq!(b.document("tube", "prefs"), None);
        assert!(b.rows().is_empty());
        assert!(
            b.publish("tube", "prefs", &change("accent", "blue"), &b_key, now())
                .is_err()
        );
        assert!(
            !b.catch_up(&b_key, a_key.verifying_key().to_bytes(), now())
                .unwrap()
                .iter()
                .any(|message| matches!(message, NeighbourMsg::AppDocument { .. }))
        );

        // Kept after the revocation, the store comes back empty.
        let kept = b.durable().unwrap();
        let restored = PersonalStore::restore(&kept, &b_cert, now()).unwrap();
        assert!(restored.rows().is_empty());
    }

    #[test]
    fn a_replayed_write_changes_nothing() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let (a_key, a_cert) = device(&root, 2, DeviceCapabilities::member());
        let mut a = PersonalStore::open(root.verifying_key().to_bytes(), a_cert, now()).unwrap();
        let published = a
            .publish("tube", "prefs", &record("theme", "dark"), &a_key, now())
            .unwrap();
        let before = a.durable().unwrap();
        let outcome = a
            .receive(&a_key, a_key.verifying_key().to_bytes(), published, now())
            .unwrap();
        assert!(outcome.duplicate);
        assert!(!outcome.changed);
        assert_eq!(a.durable().unwrap(), before);
        assert_eq!(a.rows().len(), 1);
    }

    #[test]
    fn the_store_survives_being_kept_beside_the_workspaces() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let root_key = root.verifying_key().to_bytes();
        let (a_key, a_cert) = device(&root, 2, DeviceCapabilities::member());
        let (_, b_cert) = device(&root, 3, DeviceCapabilities::member());
        let mut a = PersonalStore::open(root_key, a_cert.clone(), now()).unwrap();
        a.publish("tube", "prefs", &record("theme", "dark"), &a_key, now())
            .unwrap();
        let kept = a.durable().unwrap();

        let mut restored = PersonalStore::restore(&kept, &a_cert, now()).unwrap();
        assert_eq!(restored.store_key(), a.store_key());
        assert_eq!(restored.rows(), a.rows());
        assert_eq!(
            field(&restored.document("tube", "prefs").unwrap(), "theme").as_deref(),
            Some("dark")
        );
        // It carries on at its next sequence, and keeps what it admits.
        let published = restored
            .publish("tube", "prefs", &change("accent", "blue"), &a_key, now())
            .unwrap();
        assert!(restored.holds_document("tube", "prefs"));
        match &published {
            NeighbourMsg::AppDocument { cluster_id, .. } => {
                assert_eq!(cluster_id, &restored.store_key());
            }
            other => panic!("not a document: {other:?}"),
        }
        let again = PersonalStore::restore(&restored.durable().unwrap(), &a_cert, now()).unwrap();
        assert_eq!(again.rows(), restored.rows());

        // A copy kept by one device is not another device's to open.
        assert!(PersonalStore::restore(&kept, &b_cert, now()).is_err());
    }

    /// The write `mutation` makes of `record`, as `author`'s own store signs
    /// it: what a link carries, and what a verdict names.
    fn write(
        mutation: &DocumentMutation,
        author: &SigningKey,
        seq: u64,
        store_key: &str,
    ) -> SignedOp {
        let mut envelope = SignedOp::new_unsigned(
            mutation.encode().unwrap(),
            collab_types::document_artifact_id("tube", store_key, "prefs"),
            Ed25519PublicKey::from_bytes(author.verifying_key().to_bytes()),
            collab_types::document_attestation_id("tube", store_key, "prefs"),
            OpKind::Change,
            now(),
            seq,
        );
        collab_types::sign_op(&mut envelope, author).unwrap();
        envelope
    }

    /// A verdict on a change that later lands is let go, and its author is
    /// told so: the write arrived after all, and what this device refused is
    /// not missing from it.
    #[test]
    fn a_refused_write_that_later_lands_stops_being_missing() {
        let root = SigningKey::from_bytes(&[12; 32]);
        let (a_key, a_cert) = device(&root, 2, DeviceCapabilities::member());
        let (b_key, _b_cert) = device(&root, 3, DeviceCapabilities::member());
        let mut a = PersonalStore::open(root.verifying_key().to_bytes(), a_cert, now()).unwrap();
        let refused = write(&change("font", "serif"), &b_key, 1, &a.store_key());
        a.file_refusal(
            "tube",
            "prefs",
            &refused,
            "it waits for earlier writes",
            &a_key,
            now(),
        )
        .unwrap();
        let held = a.write_refusals();
        assert_eq!(held.len(), 1, "one verdict on the change: {held:?}");
        assert_eq!(held[0].author, b_key.verifying_key().to_bytes());
        assert_eq!(held[0].reason, RefusalReason::MissingDependency);
        assert_eq!(a.take_refusal_updates().len(), 1, "its author is told");

        // The author's next write of the route reaches this store: the change
        // the verdict named is here, and the verdict goes.
        a.landed(
            &b_key.verifying_key().to_bytes(),
            "tube",
            "prefs",
            refused.seq.saturating_add(1),
        );
        assert!(
            a.write_refusals().is_empty(),
            "the write landed after all: not missing"
        );
        let updates = a.take_refusal_updates();
        assert_eq!(updates.len(), 1, "its author is told it stands");
        assert!(
            matches!(&updates[0], NeighbourMsg::WriteRefusals { refusals } if refusals.is_empty()),
            "the set the author reads no longer names the change: {updates:?}"
        );
    }

    /// The certificate `key` holds under `root`, for a device that already
    /// sent its own.
    fn b_cert_of(root: &SigningKey, key: &SigningKey) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "device-b".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }
}
