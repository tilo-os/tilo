//! One person's own documents: the store behind `(app, personal)` resources.
//!
//! Every device of the person keeps its own copy, opened on the profile's
//! identity root. A signed write is admitted when the device that made it
//! holds a certificate the root signed, the certificate is current and the
//! device is not revoked; everything else is refused. Nothing is joined and
//! nothing is governed: the root's signature says who may write, and a
//! signed revocation says who may not.
//!
//! The documents are the engine's own — [`CachedDocument`] and
//! [`InboundState`] — admitted through the same payload validation, op
//! validation, appearance check and merge a document received for a
//! workspace goes through. An encrypted write has no place here: the key of
//! the store is the identity itself, and its documents travel only between
//! devices that already share it.
//!
//! One resource of the store is the store's own: the certificates of the
//! person's devices, as each device knows them ([`DEVICES_APP`]). A device
//! lists the certificates it holds there and admits the ones it reads there,
//! each checked as the certificate a device shows for itself is — signed by
//! the root, current, not revoked, not a store node's — so every device of
//! the person comes to know the others, whichever of them it met.
//!
//! No document, that resource included, is handed to a device this store
//! holds no such certificate of: not as a snapshot when a link starts, and
//! not as a write while it lasts. A device never met is told who this one
//! is, and catches up once the certificate it shows is admitted.

use super::{
    DocumentRow, PeerDocument, ReceiveOutcome, Route, backend, default_surfaces, err, now_ns,
    peer_handle,
};
use crate::AdmissionError;
use crate::NeighbourMsg;
use crate::admission::{DocumentAdmission, ExactAdmissions};
use crate::cached_document::CachedDocument;
use crate::verified_certificate::VerifiedCertificate;
use automerge::AutoCommit;
use automerge::transaction::Transactable as _;
use collab_types::signer::DeviceSigner;
use collab_types::{
    ArtifactKind, DocumentMutation, Ed25519PublicKey, InboundState, OpKind, SignedOp,
    WholeDocumentPayload,
};
use neighbour_wire::{RefusalReason, WriteRefusal};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use tilo_action_policy::{
    Acting, ActionKind, Answer, Authority, Context, Grantee, Party, TreePath, WorkspacePolicy,
};
use tilo_lock::MutexRecover;
use tilo_user_identity::{DeviceCert, RevocationEntry, RevocationList};
use web_time::{Duration, SystemTime};

pub mod device_grants;
pub mod link;
pub mod machines;
pub mod scopes;

use scopes::resource_owner;

/// The version of [`PersonalStore::durable`] this build writes.
const STORE_VERSION: u32 = 3;
/// The version it held before devices kept each other's own resources, which
/// this build still opens: with none kept to hand on.
const STORE_VERSION_UNOWNED: u32 = 2;

/// A write's clock may lead or trail the receiving device by this much.
const CLOCK_SKEW: Duration = Duration::from_secs(300);

/// The app the resource of the person's devices is filed under: the identity's
/// own, which no installed app publishes resources as.
const DEVICES_APP: &str = "so.tilo.identity";
/// The kind of the person's resource that lists their devices: one entry per
/// device, its key in hex naming its certificate's postcard bytes.
const DEVICES_KIND: &str = "identity.devices";
/// The kind of the person's resource that lists their workspaces: which they
/// have, which of their devices holds, hosts and shows each, and where each
/// is hosted (`tilo_user_identity::WorkspaceDirectory` reads and writes it).
pub const WORKSPACES_KIND: &str = "identity.workspaces";
/// The kind of the person's resource that holds the Wi-Fi passphrases their
/// devices share: one sealed credential per network, under its id in hex.
pub const WIFI_KIND: &str = "identity.wifi";
/// The name of the part of a device's own subtree that holds what the device
/// says of itself that lasts: what it is, its speaker groups and its disk
/// unlock, each under its own key.
pub const DEVICE_FACTS: &str = "facts";
/// The kind of the person's resource that holds the machines that asked to
/// join and have not been answered yet: one entry per ask, under its id, and
/// the answer any of their devices wrote. The storing device's session shows
/// it from here and carries out whatever answer arrives.
pub const ENROLLMENT_KIND: &str = "identity.enrollment";
/// The most devices that resource names to this device: entries past it are
/// not read, and this device adds none.
const MAX_DEVICES: usize = 256;
/// The longest certificate one entry of it carries.
const MAX_CERTIFICATE_BYTES: usize = 4096;
/// The most writes of one entry, made at once by different devices, that are
/// read for the certificate it lists.
const MAX_WRITES: usize = 8;

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
    /// Whether the certificates this device holds moved since the resource of
    /// the person's devices was last published: the links publish it again.
    devices_moved: bool,
    /// The newest whole state each device signed of a resource of its own
    /// subtree ([`scopes`]), as that device signed it: what this device hands
    /// on to a sibling the owner is not linked to, unchanged.
    owned: BTreeMap<Route, SignedOp>,
}

/// Where a device keeping its state in `state` keeps the person's own
/// documents: one file, the whole store.
#[must_use]
pub fn store_path(state: &std::path::Path) -> std::path::PathBuf {
    state.join("tilo").join("personal").join("documents")
}

/// Where a write a store decides came from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// This device signed it.
    Local,
    /// The device on the link signed it.
    Sent,
    /// The device on the link hands on what another device signed of its
    /// own subtree.
    Relayed,
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
            // This device's own certificate is not listed yet.
            devices_moved: true,
            owned: BTreeMap::new(),
        })
    }

    /// The person's store a device keeps under `state`: the store an earlier
    /// start wrote is read back, and the first start opens it empty on the
    /// person's root, held by `device`.
    ///
    /// A machine copied from another re-keyed its device: the store kept
    /// there is the device it was copied from, which still holds it. It is
    /// set aside, never opened as this device's, and the person's documents
    /// come back from their other devices as to any new one of theirs.
    ///
    /// # Errors
    ///
    /// The store kept there cannot be read, or `device` is not a certificate
    /// the person's root signed that still holds.
    pub fn open_at(
        state: &std::path::Path,
        root: [u8; 32],
        device: DeviceCert,
        now: SystemTime,
    ) -> Result<Self, String> {
        let path = store_path(state);
        match std::fs::read(&path) {
            Ok(bytes)
                if Self::kept_by(&bytes).is_some_and(|kept| kept != device.device_public_key) =>
            {
                let kept = Self::kept_by(&bytes).unwrap_or_default();
                let aside = path.with_extension(format!("of-{}", hex::encode(&kept[..4])));
                std::fs::rename(&path, &aside)
                    .map_err(|error| format!("{}: {error}", path.display()))?;
                Self::open(root, device, now)
            }
            Ok(bytes) => Self::restore(&bytes, &device, now),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Self::open(root, device, now)
            }
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }

    /// The identity root the store opens on.
    #[must_use]
    pub fn root(&self) -> [u8; 32] {
        self.root
    }

    /// The store's key in the routing key's text form: the cluster every
    /// resource of this person's own is routed under.
    ///
    /// It is derived from the identity root alone, so two devices of one
    /// person agree on where a document lives without either naming the
    /// other, and an app's own route for one of its resources names the same
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
        resource: &str,
        envelope: &SignedOp,
        refused: &AdmissionError,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<(), String> {
        if key.device_public_key() != self.device.device_public_key {
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
        if self.revocations.is_revoked(&self.root, &author) {
            self.refusals.retain(|held| {
                !(held.by == self.device.device_public_key && held.author == author)
            });
        }
        let mut refusal = WriteRefusal {
            workspace: self.store_key(),
            app: app.into(),
            record: resource.into(),
            author,
            seq: envelope.seq,
            operation: format!("{}:{}", envelope.artifact.0, envelope.seq),
            changes: self.unheld_changes(app, resource, envelope),
            reason: refused.reason(),
            detail: refused.to_string(),
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
    fn unheld_changes(&self, app: &str, resource: &str, envelope: &SignedOp) -> Vec<[u8; 32]> {
        let mut changes = change_hashes(&envelope.payload);
        let Some(held) = self.documents.get(&Self::route(app, resource)) else {
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
            self.admits_device(&source, now)
                .map_err(|error| error.to_string())?;
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
        let held = self
            .certificates
            .insert(certificate.device_public_key, verified);
        // A certificate this device did not hold is one its siblings may
        // lack: the resource of the person's devices is published with it.
        self.devices_moved |= held.as_ref().is_none_or(|held| **held != *certificate);
        Ok(held.is_none())
    }

    /// The route of the resource that lists the person's devices.
    fn devices_route() -> Route {
        Self::route(DEVICES_APP, &collab_types::person_document_id(DEVICES_KIND))
    }

    /// Whether `resource` of `app` is the resource that lists the person's
    /// devices: the store's own, which no app reads or is handed.
    fn is_devices_document(app: &str, resource: &str) -> bool {
        app == DEVICES_APP && resource == collab_types::person_document_id(DEVICES_KIND)
    }

    /// Whether this store holds a certificate for `device` that makes it one
    /// of the person's devices now: one their root signed, current, and not
    /// revoked. A device never met holds none, whatever it will show.
    fn holds_device(&self, device: &[u8; 32], now: SystemTime) -> bool {
        self.certificates.get(device).is_some_and(|certificate| {
            !self.revocations.refuses(certificate) && certificate.current_typed(now_ns(now)).is_ok()
        })
    }

    /// Whether `message` is `remote`'s to be sent. A document of the person's
    /// goes only to a device already verified as one of theirs: a device that
    /// has shown no certificate, one of another person's, a revoked one and
    /// one whose certificate ran out are sent none, the resource of the
    /// person's devices included. What this device refuses is told to the
    /// device whose writes it is about and to the person's devices, and to
    /// no one else: a refusal names the resource it is of.
    #[cfg_attr(
        target_arch = "wasm32",
        allow(dead_code, reason = "the page's session wires its own links")
    )]
    fn hands(&self, message: &NeighbourMsg, remote: &[u8; 32], now: SystemTime) -> bool {
        match message {
            NeighbourMsg::AppDocument { .. } => self.holds_device(remote, now),
            NeighbourMsg::WriteRefusals { refusals } => {
                self.holds_device(remote, now)
                    || refusals.iter().all(|refusal| &refusal.author == remote)
            }
            _ => true,
        }
    }

    /// The certificates `doc` lists, at most [`MAX_DEVICES`] entries of it
    /// read: each under the key of the device it certifies. Nothing here is
    /// verified; an entry that is no certificate, or not that device's, is
    /// no entry.
    fn listed_devices(doc: &AutoCommit) -> Vec<DeviceCert> {
        use automerge::ReadDoc as _;
        doc.keys(automerge::ROOT)
            .take(MAX_DEVICES)
            .filter_map(|device| {
                // ignored-failure: an entry that cannot be read lists no device
                let written = doc.get_all(automerge::ROOT, device.as_str()).ok()?;
                // Two devices that listed one device at once both wrote its
                // entry: the later certificate is the one listed, whichever
                // write the document settled on.
                written
                    .iter()
                    .take(MAX_WRITES)
                    .filter_map(|(value, _)| Self::listed_certificate(value, &device))
                    .max_by_key(|certificate| certificate.issued_at_ns)
            })
            .collect()
    }

    /// The certificate of the device keyed `device` that `value` is, when it
    /// is one: nothing of it is verified here.
    fn listed_certificate(value: &automerge::Value<'_>, device: &str) -> Option<DeviceCert> {
        let automerge::Value::Scalar(value) = value else {
            return None;
        };
        let automerge::ScalarValue::Bytes(bytes) = value.as_ref() else {
            return None;
        };
        if bytes.len() > MAX_CERTIFICATE_BYTES {
            return None;
        }
        // ignored-failure: bytes that are no certificate list no device
        let certificate: DeviceCert = postcard::from_bytes(bytes).ok()?;
        (hex::encode(certificate.device_public_key) == device).then_some(certificate)
    }

    /// The certificates this device holds, of devices that are the person's
    /// now, that the resource of their devices does not list or lists an
    /// earlier certificate for: what this device adds to it, while it has
    /// room.
    fn unlisted_devices(&self, now: SystemTime) -> Vec<DeviceCert> {
        use automerge::ReadDoc as _;
        let (listed, entries) = self
            .documents
            .get(&Self::devices_route())
            .map(|held| {
                (
                    Self::listed_devices(&held.doc),
                    held.doc.length(automerge::ROOT),
                )
            })
            .unwrap_or_default();
        let mut room = MAX_DEVICES.saturating_sub(entries);
        self.certificates
            .iter()
            .filter(|(device, _)| self.holds_device(device, now))
            .filter(|(device, certificate)| {
                match listed
                    .iter()
                    .find(|listed| &listed.device_public_key == *device)
                {
                    Some(listed) => listed.issued_at_ns < certificate.issued_at_ns,
                    None if room == 0 => false,
                    None => {
                        room = room.saturating_sub(1);
                        true
                    }
                }
            })
            .map(|(_, certificate)| certificate.as_ref().clone())
            .collect()
    }

    /// Admit the devices the resource of the person's devices lists that this
    /// one does not hold, or holds an earlier certificate of. Each is
    /// admitted as the certificate a device shows for itself is: one the
    /// root did not sign, one that does not hold now, a revoked device's and
    /// a store node's stay out. Returns whether any was new here.
    fn learn_devices(&mut self, now: SystemTime) -> bool {
        let Some(listed) = self
            .documents
            .get(&Self::devices_route())
            .map(|held| Self::listed_devices(&held.doc))
        else {
            return false;
        };
        let mut learned = false;
        for certificate in listed {
            let device = certificate.device_public_key;
            // This device's own certificate is the one its identity gave it.
            if device == self.device.device_public_key {
                continue;
            }
            if self
                .certificates
                .get(&device)
                .is_some_and(|held| held.issued_at_ns >= certificate.issued_at_ns)
            {
                continue;
            }
            // ignored-failure: a listed certificate this store refuses is one it does not admit
            learned |= self.admit_device(&certificate, now).is_ok();
        }
        learned
    }

    /// The resource of the person's devices was written: admit what it lists,
    /// and have it published again when it lacks what this device holds.
    /// Returns whether a device was learned from it.
    fn devices_written(&mut self, now: SystemTime) -> bool {
        let learned = self.learn_devices(now);
        self.devices_moved |= !self.unlisted_devices(now).is_empty();
        learned
    }

    /// The resource of the person's devices as the frame to publish, when the
    /// certificates this device holds moved since it last was: with every
    /// one of them it lacked, whole, so a device that never held the resource
    /// reads it as well as one that did. `None` while nothing moved, and on
    /// a device the person revoked.
    fn take_devices_update(
        &mut self,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<Option<NeighbourMsg>, String> {
        use collab_types::CommitNamed as _;
        if !std::mem::take(&mut self.devices_moved) || self.revoked_here() {
            return Ok(None);
        }
        let route = Self::devices_route();
        let mut doc = match self.documents.get(&route) {
            Some(held) => AutoCommit::load(&held.doc.saved()).map_err(err)?,
            None => AutoCommit::new(),
        };
        for certificate in self.unlisted_devices(now) {
            doc.put(
                automerge::ROOT,
                hex::encode(certificate.device_public_key),
                automerge::ScalarValue::Bytes(postcard::to_allocvec(&certificate).map_err(err)?),
            )
            .map_err(err)?;
        }
        doc.commit_edit(DEVICES_KIND, "devices");
        self.publish(
            &route.app,
            &route.record,
            &DocumentMutation::Snapshot(doc.save()),
            key,
            now,
        )
        .map(Some)
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
    ///
    /// # Errors
    /// [`RefusalReason::Revoked`] for a revoked device, and
    /// [`RefusalReason::Expired`] or [`RefusalReason::NotCertified`] for a
    /// held certificate that does not hold now.
    pub fn admits_device(&self, device: &[u8; 32], now: SystemTime) -> Result<(), AdmissionError> {
        if self.revocations.is_revoked(&self.root, device) {
            return Err(AdmissionError::rejected(
                RefusalReason::Revoked,
                "device revoked",
            ));
        }
        if let Some(certificate) = self.certificates.get(device) {
            certificate.current_typed(now_ns(now)).map_err(|error| {
                let reason = match error {
                    tilo_user_identity::DeviceCertError::Expired => RefusalReason::Expired,
                    _ => RefusalReason::NotCertified,
                };
                AdmissionError::rejected(reason, error)
            })?;
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

    /// Whether this store holds `resource` of `app`.
    #[must_use]
    pub fn holds_document(&self, app: &str, resource: &str) -> bool {
        self.documents.contains_key(&Self::route(app, resource))
    }

    /// The whole document, as a device receiving it would save it.
    #[must_use]
    pub fn document(&self, app: &str, resource: &str) -> Option<Vec<u8>> {
        self.documents
            .get(&Self::route(app, resource))
            .map(|held| held.doc.saved())
    }

    /// Every document held, by what it belongs to.
    #[must_use]
    pub fn rows(&self) -> Vec<DocumentRow> {
        self.documents
            .iter()
            // The identity's own resources — the person's devices and their
            // workspaces — are the store's, which no app reads.
            .filter(|(route, _)| route.app != DEVICES_APP)
            .map(|(route, held)| DocumentRow {
                app_id: route.app.clone(),
                document_id: route.record.clone(),
                heads: held
                    .doc
                    .base_heads()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            })
            .collect()
    }

    /// The person's own resource of `kind` ([`WORKSPACES_KIND`],
    /// [`WIFI_KIND`]) as this device holds it: the whole document. `None`
    /// while no device of theirs wrote it. No app reads these: the session
    /// keeps them for the person.
    #[must_use]
    pub fn person_resource(&self, kind: &str) -> Option<Vec<u8>> {
        self.document(DEVICES_APP, &collab_types::person_document_id(kind))
    }

    /// Write `mutation` of the person's own resource of `kind`: signed with
    /// `key`, admitted here, and returned as the frame for their other
    /// devices.
    pub fn publish_person_resource(
        &mut self,
        kind: &str,
        mutation: &DocumentMutation,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        self.publish(
            DEVICES_APP,
            &collab_types::person_document_id(kind),
            mutation,
            key,
            now,
        )
    }

    /// The resource `name` of `device`'s own subtree ([`scopes`]) as this
    /// device holds it: what that device alone wrote, whole. `None` while it
    /// wrote none, or none reached this device.
    #[must_use]
    pub fn device_part(&self, device: &[u8; 32], name: &str) -> Option<Vec<u8>> {
        self.document(DEVICES_APP, &scopes::device_resource(device, name))
    }

    /// Write `mutation` of the resource `name` of this device's own subtree:
    /// signed with `key`, admitted here, and returned as the frame for the
    /// person's other devices.
    pub fn publish_device_part(
        &mut self,
        name: &str,
        mutation: &DocumentMutation,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        let resource = scopes::device_resource(&self.device.device_public_key, name);
        self.publish(DEVICES_APP, &resource, mutation, key, now)
    }

    fn route(app: &str, resource: &str) -> Route {
        Route {
            app: app.into(),
            record: resource.into(),
        }
    }

    /// Sign `mutation` of `resource` with this device's key, admit it, and
    /// return the frame to publish to the person's other devices.
    pub fn publish(
        &mut self,
        app: &str,
        resource: &str,
        mutation: &DocumentMutation,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        let author = Ed25519PublicKey::from_bytes(key.device_public_key());
        let signed = self.sign(app, resource, mutation, &author, key, now)?;
        self.admit_local(app, resource, signed, key, now)
    }

    /// Admit a write this device signed itself: what an app publishing one of
    /// its own resources does, as `admit_local_app_document` does in a
    /// workspace. Returns the frame to publish to the person's other devices.
    pub fn admit_local(
        &mut self,
        app: &str,
        resource: &str,
        envelope: SignedOp,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        if envelope.author.as_bytes() != &self.device.device_public_key {
            return Err("this device signs its own writes".into());
        }
        let mut candidate = self.clone();
        let refused = candidate
            .write(app, resource, &envelope, now, Origin::Local)
            .err();
        *self = candidate;
        if let Some(refused) = refused {
            // A write this device could not decide is no verdict on it.
            if refused.is_device_failure() {
                return Err(refused.to_string());
            }
            // The store's verdict on the person's own write is filed like a
            // sibling's: what its history shows, and what a notice is made of.
            // The resource of their devices is the store's, not a write of
            // theirs: nobody is told of it.
            if !Self::is_devices_document(app, resource) {
                self.file_refusal(app, resource, &envelope, &refused, key, now)?;
            }
            return Err(refused.to_string());
        }
        // A change of this device's own subtree goes out as the whole document
        // it leaves, signed here: what a sibling keeps to hand on for this
        // device, which a change alone could not be.
        if resource_owner(resource) == Some(self.device.device_public_key)
            && matches!(
                DocumentMutation::decode(&envelope.payload),
                Ok(DocumentMutation::Incremental(_))
            )
            && let Some(whole) = self.document(app, resource)
        {
            let author = Ed25519PublicKey::from_bytes(self.device.device_public_key);
            let snapshot = self.sign(
                app,
                resource,
                &DocumentMutation::Snapshot(whole),
                &author,
                key,
                now,
            )?;
            self.write(app, resource, &snapshot, now, Origin::Local)
                .map_err(|refused| refused.to_string())?;
            return Ok(self.frame(app, resource, snapshot));
        }
        Ok(self.frame(app, resource, envelope))
    }

    /// What this device says first to whoever is on a link, before that
    /// device has shown anything: its certificate and the revocations its
    /// identity signed, and what it refuses of that device's writes. Nothing
    /// of the person's documents is in it.
    fn greeting(&self, remote: &[u8; 32]) -> Vec<NeighbourMsg> {
        let mut messages = vec![self.hello()];
        // What this device refuses of the peer's writes reaches it on the
        // link, live or here, even when it is revoked and gets nothing else:
        // a person whose device was taken away is told why their writes stop.
        if let Some(frame) = self.refusals_frame(remote) {
            messages.push(frame);
        }
        messages
    }

    /// A signed snapshot of every document held, for `remote` to catch up
    /// from: whoever holds a document can hand over its whole state, so a
    /// device added later catches up from any of its siblings. `None` while
    /// this store holds no certificate that makes `remote` one of the
    /// person's devices now: a device that has shown none, one of another
    /// person's, a revoked one and one whose certificate ran out are handed
    /// no document.
    fn documents_for(
        &mut self,
        key: &dyn DeviceSigner,
        remote: &[u8; 32],
        now: SystemTime,
    ) -> Result<Option<Vec<NeighbourMsg>>, String> {
        if !self.holds_device(remote, now) {
            return Ok(None);
        }
        // A revoked device says only who it is and what was refused of
        // theirs: its siblings keep no documents from it.
        if self.revoked_here() {
            return Ok(Some(Vec::new()));
        }
        let routes: Vec<Route> = self.documents.keys().cloned().collect();
        let mut messages = Vec::with_capacity(routes.len());
        for route in routes {
            // Another device's own resource is handed on as that device signed
            // it, or not at all: this device cannot sign it.
            if let Some(owner) = resource_owner(&route.record)
                && owner != self.device.device_public_key
            {
                if let Some(envelope) = self.owned.get(&route) {
                    messages.push(self.frame(&route.app, &route.record, envelope.clone()));
                }
                continue;
            }
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
        Ok(Some(messages))
    }

    /// What another device may lack: this device's certificate, the
    /// revocations its identity signed and what it refuses of that device's
    /// writes, then, for a device this store holds a certificate of that
    /// makes it one of the person's now, a signed snapshot of every document
    /// held. A device never met is given the first part alone: its documents
    /// follow once the certificate it shows is admitted.
    pub fn catch_up(
        &mut self,
        key: &dyn DeviceSigner,
        remote: [u8; 32],
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let mut messages = self.greeting(&remote);
        messages.extend(self.documents_for(key, &remote, now)?.unwrap_or_default());
        Ok(messages)
    }

    /// Admit a frame `source` sent, `source` being the device key its
    /// connection proved.
    pub fn receive(
        &mut self,
        key: &dyn DeviceSigner,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<ReceiveOutcome, String> {
        if key.device_public_key() != self.device.device_public_key {
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
        key: &dyn DeviceSigner,
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
                // A device at a peer protocol version this one does not work
                // with is admitted to nothing: what it says is not read here.
                if !neighbour_wire::works_with_peer_version(protocol) {
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
        // told what it was refused instead of being dropped at the API: a
        // person whose device was taken away learns why their writes stop.
        if !self.revocations.is_revoked(&self.root, &source) {
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
        key: &dyn DeviceSigner,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<ReceiveOutcome, String> {
        let NeighbourMsg::AppDocument {
            peer,
            app_id,
            cluster_id,
            document_id,
            envelope,
            ..
        } = message
        else {
            return Err("not a document message".into());
        };
        let author = *envelope.author.as_bytes();
        // A device hands on another's own resource only as its owner signed it,
        // and only once it is one of the person's devices itself.
        let origin = if author == source {
            Origin::Sent
        } else if resource_owner(&document_id) == Some(author) && self.holds_device(&source, now) {
            Origin::Relayed
        } else {
            return Err("transport author or store key mismatch".to_owned());
        };
        let refused = if cluster_id != self.store_key() || peer != peer_handle(&author) {
            AdmissionError::forbidden("transport author or store key mismatch")
        } else {
            match self.write(&app_id, &document_id, &envelope, now, origin) {
                Ok(outcome) => return Ok(outcome),
                Err(reason) => reason,
            }
        };
        // What a device hands on is no write of its own, and its author did
        // not send it here: nobody is told of a refusal of it.
        if origin == Origin::Relayed {
            return Err(refused.to_string());
        }
        // A write this device could not decide is no verdict on it: nothing is
        // filed, and its author's next write proceeds.
        if refused.is_device_failure() {
            return Err(refused.to_string());
        }
        // The store's verdict on this one write is now a fact it keeps: the
        // write's author is told, and its history joins it to the write. The
        // resource of the person's devices is no write of theirs to be told of.
        if !Self::is_devices_document(&app_id, &document_id) {
            self.file_refusal(&app_id, &document_id, &envelope, &refused, key, now)?;
        }
        Err(refused.to_string())
    }

    /// The one admission: the signed write is the author's own, its device is
    /// certified by the root and live, its route is this store, and what it
    /// changes merges into the resource. Everything is decided in a copy, so a
    /// refusal leaves the store as it was.
    /// What `envelope` changes of `resource`: a personal document's change is
    /// cleartext between the person's devices, and nothing but a change.
    fn cleartext_change(
        app: &str,
        store_key: &str,
        resource: &str,
        envelope: &SignedOp,
    ) -> Result<DocumentMutation, AdmissionError> {
        let payload =
            collab_types::validate_whole_document_payload_route(app, store_key, resource, envelope)
                .map_err(AdmissionError::malformed)?;
        let WholeDocumentPayload::Cleartext(mutation) = payload else {
            return Err(AdmissionError::malformed(
                "a personal document is cleartext between its devices",
            ));
        };
        if envelope.kind != OpKind::Change {
            return Err(AdmissionError::forbidden(
                "the personal store admits document changes",
            ));
        }
        Ok(mutation)
    }

    /// Whether a write `source` signed may be decided here at all: this
    /// device still holds the store, a write of its own is signed by it, and
    /// another device's is a certified device the person has not revoked.
    fn admits_author(
        &self,
        source: [u8; 32],
        origin: Origin,
        now: SystemTime,
    ) -> Result<(), AdmissionError> {
        if self.revoked_here() {
            return Err(AdmissionError::rejected(
                RefusalReason::Revoked,
                "this device was revoked",
            ));
        }
        if origin == Origin::Local {
            if source != self.device.device_public_key {
                return Err(AdmissionError::forbidden(
                    "this device signs its own writes",
                ));
            }
            return Ok(());
        }
        self.admits_device(&source, now)?;
        if !self.certificates.contains_key(&source) {
            return Err(AdmissionError::rejected(
                RefusalReason::NotCertified,
                "the device's certificate is not here",
            ));
        }
        Ok(())
    }

    fn write(
        &mut self,
        app: &str,
        resource: &str,
        envelope: &SignedOp,
        now: SystemTime,
        origin: Origin,
    ) -> Result<ReceiveOutcome, AdmissionError> {
        let source = *envelope.author.as_bytes();
        self.admits_author(source, origin, now)?;
        let store_key = self.store_key();
        let admitted = DocumentAdmission::from_op(&store_key, app, resource, envelope)
            .map_err(AdmissionError::malformed)?;
        if self.exact.get(&admitted.key) == Some(&admitted.operation_id) {
            return Ok(ReceiveOutcome {
                duplicate: true,
                ..Default::default()
            });
        }
        let mutation = Self::cleartext_change(app, &store_key, resource, envelope)?;
        let route = Self::route(app, resource);
        let route_existed = self.documents.contains_key(&route);
        if !route_existed && matches!(mutation, DocumentMutation::Incremental(_)) {
            return Err(AdmissionError::rejected(
                RefusalReason::MissingDependency,
                "the change arrived before the document it changes",
            ));
        }
        let owner = resource_owner(resource);
        if origin == Origin::Relayed {
            if !matches!(mutation, DocumentMutation::Snapshot(_)) {
                return Err(AdmissionError::malformed(
                    "a device's own resource is handed on whole",
                ));
            }
            if self.holds_newer(&route, envelope) {
                return Ok(ReceiveOutcome {
                    duplicate: true,
                    ..Default::default()
                });
            }
        }
        let mut candidate = self.candidate(&route, envelope, &store_key);
        let mut validated = candidate.channel.clone();
        validated.surfaces = default_surfaces();
        // The person owns every node of their own store, and each device acts
        // as itself in it: a resource of one device's own subtree is that
        // device's alone, and every other write is decided as the edit of its
        // document that it is.
        let author = OwnStoreAuthor {
            device: hex::encode(source),
            store: &store_key,
            subtree: owner,
        };
        // What a device hands on was signed when its owner wrote it, which may
        // be long before: its age is no reason to refuse it.
        let validate = if origin == Origin::Relayed {
            collab_types::validate_replayed_inbound_op
        } else {
            collab_types::validate_admitted_inbound_op
        };
        validate(envelope, &author, &mut validated, now, CLOCK_SKEW)
            .map_err(|error| format!("the write is not admitted: {error}"))
            .map_err(AdmissionError::malformed)?;
        candidate.channel = validated;
        collab_types::check_claimed(envelope).map_err(AdmissionError::malformed)?;
        if let DocumentMutation::Incremental(bytes) = &mutation {
            let changes = collab_types::parse_changes(bytes).map_err(AdmissionError::malformed)?;
            collab_types::check_appearances(&changes, None).map_err(|refusal| {
                AdmissionError::forbidden(format!("document change forbidden: {refusal}"))
            })?;
        }
        let changed =
            match crate::document::merge_committed_with_status(&mut candidate.doc, &mutation)
                .map_err(AdmissionError::from)?
            {
                crate::document::MergeOutcome::Changed(changed) => changed,
                crate::document::MergeOutcome::MissingDependencies => {
                    return Err(AdmissionError::rejected(
                        RefusalReason::MissingDependency,
                        "the change's earlier writes are not here",
                    ));
                }
            };
        self.exact
            .insert(&admitted)
            .map_err(AdmissionError::malformed)?;
        // The owner's newest whole state of its own resource is what this
        // device hands on for it.
        if owner == Some(source)
            && matches!(mutation, DocumentMutation::Snapshot(_))
            && self
                .owned
                .get(&route)
                .is_none_or(|kept| kept.seq < envelope.seq)
        {
            self.owned.insert(route.clone(), envelope.clone());
        }
        self.documents.insert(route, candidate);
        self.landed(&source, app, resource, envelope.seq);
        let learned = Self::is_devices_document(app, resource) && self.devices_written(now);
        Ok(ReceiveOutcome {
            changed,
            metadata_changed: changed || learned,
            ..Default::default()
        })
    }

    /// Whether this device holds a state of `route` its author signed as new
    /// as `envelope` or newer: an older one handed on late is no news.
    fn holds_newer(&self, route: &Route, envelope: &SignedOp) -> bool {
        self.documents
            .get(route)
            .and_then(|held| held.channel.last_seq_per_author.get(&envelope.author))
            .is_some_and(|seen| *seen >= envelope.seq)
    }

    /// The document `route` names as a write `envelope` makes of it starts
    /// from: the one held, or an empty one, routed in the store `store_key`.
    fn candidate(&self, route: &Route, envelope: &SignedOp, store_key: &str) -> PeerDocument {
        let mut candidate = self
            .documents
            .get(route)
            .cloned()
            .unwrap_or_else(|| PeerDocument {
                doc: AutoCommit::new().into(),
                channel: InboundState::default(),
                encryption_required: false,
                covered: BTreeMap::new(),
                covered_snapshot: None,
                sealed: backend::SealedLog::default(),
                resealed: std::sync::Arc::default(),
            });
        let state = &mut candidate.channel;
        state
            .artifacts
            .insert(envelope.artifact, ArtifactKind::Note);
        state
            .artifact_apps
            .insert(envelope.artifact, route.app.clone());
        state
            .artifact_resources
            .insert(envelope.artifact, route.record.clone());
        store_key.clone_into(&mut state.workspace);
        candidate
    }

    /// The author's whole state is offered on every link, so a write at or
    /// past a refused one on the same route means that one landed too: what
    /// this device refused of this author is cleared.
    fn landed(&mut self, author: &[u8; 32], app: &str, resource: &str, seq: u64) {
        let before = self.refusals.len();
        self.refusals.retain(|refusal| {
            !(refusal.by == self.device.device_public_key
                && refusal.author == *author
                && refusal.app == app
                && refusal.record == resource
                && refusal.seq <= seq)
        });
        self.refusals_moved |= self.refusals.len() != before;
    }

    fn sign(
        &self,
        app: &str,
        resource: &str,
        mutation: &DocumentMutation,
        author: &Ed25519PublicKey,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<SignedOp, String> {
        if author.as_bytes() != &self.device.device_public_key {
            return Err("this device signs its own writes".into());
        }
        let store_key = self.store_key();
        let seq = self
            .documents
            .get(&Self::route(app, resource))
            .and_then(|held| held.channel.last_seq_per_author.get(author))
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("sequence exhausted")?;
        let mut op = SignedOp::new_unsigned(
            mutation.encode().map_err(err)?,
            collab_types::document_artifact_id(app, &store_key, resource),
            Ed25519PublicKey::from_bytes(*author.as_bytes()),
            collab_types::document_attestation_id(app, &store_key, resource),
            OpKind::Change,
            now,
            seq,
        );
        collab_types::sign_op(&mut op, key).map_err(err)?;
        Ok(op)
    }

    fn frame(&self, app: &str, resource: &str, envelope: SignedOp) -> NeighbourMsg {
        NeighbourMsg::AppDocument {
            peer: peer_handle(envelope.author.as_bytes()),
            app_id: app.into(),
            cluster_id: self.store_key(),
            document_id: resource.into(),
            envelope: Box::new(envelope),
            // The person's own store holds only theirs: none is a member's own
            // resource of a workspace.
            readers: None,
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
            owned: self
                .owned
                .iter()
                .map(|(route, envelope)| DurableOwned {
                    app: route.app.clone(),
                    record: route.record.clone(),
                    envelope: envelope.clone(),
                })
                .collect(),
        };
        postcard::to_allocvec(&durable).map_err(err)
    }

    /// The device that kept the store `bytes` holds, when they are a store
    /// this build reads.
    #[must_use]
    pub fn kept_by(bytes: &[u8]) -> Option<[u8; 32]> {
        tilo_failure::ignored!(
            "bytes this build cannot read name no device, and `restore` reports why",
            postcard::from_bytes::<Durable>(bytes)
                .map(|durable| durable.device.device_public_key)
                .or_else(|_| {
                    postcard::from_bytes::<DurableUnowned>(bytes)
                        .map(|unowned| unowned.device.device_public_key)
                })
                .ok()
        )
    }

    /// Open the store `bytes` holds, as `device`, which must be the device
    /// that kept them.
    pub fn restore(bytes: &[u8], device: &DeviceCert, now: SystemTime) -> Result<Self, String> {
        // A store kept before devices kept each other's own resources is read
        // as one keeping none: the person's documents are never lost to a
        // field added later, and each device hands its own on again.
        let durable: Durable = match postcard::from_bytes(bytes) {
            Ok(durable) => durable,
            Err(error) => {
                let unowned: DurableUnowned = postcard::from_bytes(bytes).map_err(err)?;
                if unowned.version != STORE_VERSION_UNOWNED {
                    return Err(error.to_string());
                }
                Durable {
                    version: STORE_VERSION_UNOWNED,
                    root: unowned.root,
                    device: unowned.device,
                    certificates: unowned.certificates,
                    revocations: unowned.revocations,
                    documents: unowned.documents,
                    exact: unowned.exact,
                    refusals: unowned.refusals,
                    told: unowned.told,
                    owned: Vec::new(),
                }
            }
        };
        if durable.version != STORE_VERSION && durable.version != STORE_VERSION_UNOWNED {
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
            // A sibling's certificate that stopped holding since the store
            // was kept admits nothing, and the store opens without it: the
            // person's documents are not lost to another device's expiry.
            // ignored-failure: a kept certificate that no longer holds is not admitted
            let _ = store.admit_device(&certificate, now);
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
            let mut channel = InboundState::default();
            channel.artifacts.insert(artifact, ArtifactKind::Note);
            channel.artifact_apps.insert(artifact, document.app);
            channel.artifact_resources.insert(artifact, document.record);
            channel.workspace.clone_from(&store_key);
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
                    resealed: std::sync::Arc::default(),
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
        for kept in durable.owned {
            let route = Self::route(&kept.app, &kept.record);
            // Only a document held here, and its owner's own signature of it.
            if store.documents.contains_key(&route)
                && resource_owner(&kept.record) == Some(*kept.envelope.author.as_bytes())
                && collab_types::verify_op(&kept.envelope).is_ok()
            {
                store.owned.insert(route, kept.envelope);
            }
        }
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
    tilo_failure::ignored!(
        "a payload that is not automerge bytes names no changes this device lacks",
        collab_types::parse_changes(&bytes).unwrap_or_default()
    )
    .iter()
    .map(automerge::Change::hash)
    .map(|hash| hash.0)
    .collect()
}

/// A device's certificate as the store admits it: one this store's root
/// signed, and current.
fn admitted(
    root: [u8; 32],
    certificate: &DeviceCert,
    now: SystemTime,
) -> Result<VerifiedCertificate, String> {
    if certificate.user_public_key != root {
        return Err("the certificate is not this identity's".into());
    }
    VerifiedCertificate::new(certificate.clone(), now_ns(now))
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
    owned: Vec<DurableOwned>,
}

/// The store as it was kept before devices kept each other's own resources:
/// this build reads it so a person's own documents survive the field being
/// added.
#[derive(Serialize, Deserialize)]
struct DurableUnowned {
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

/// A device's own resource as its owner last signed it whole.
#[derive(Serialize, Deserialize)]
struct DurableOwned {
    app: String,
    record: String,
    envelope: SignedOp,
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

    /// Whether this device holds `resource` of `app`.
    #[must_use]
    pub fn holds_document(&self, app: &str, resource: &str) -> bool {
        self.read(|store| store.holds_document(app, resource))
    }

    /// The whole document, as a device receiving it would save it.
    #[must_use]
    pub fn document(&self, app: &str, resource: &str) -> Option<Vec<u8>> {
        self.read(|store| store.document(app, resource))
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
        resource: &str,
        envelope: SignedOp,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        self.write(|store| store.admit_local(app, resource, envelope, key, now))
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

/// Who a write to the person's own store is decided as: the device that
/// signed it owns every node of the store, and a change naming its invoker (an
/// agent working for the device) is decided as that invoker too,
/// bounded by the device, through the same decision a workspace takes.
struct OwnStoreAuthor<'a> {
    /// The signing device, as the store names its owner.
    device: String,
    /// The store's key: the workspace its nodes are under.
    store: &'a str,
    /// The device whose own subtree the written resource is in, when it is in one
    /// ([`resource_owner`]): that device's alone to write.
    subtree: Option<[u8; 32]>,
}

impl collab_types::ChangeAuthor for OwnStoreAuthor<'_> {
    fn answer(
        &self,
        invoker: Option<&Acting>,
        node: &TreePath,
        kind: ActionKind,
        cx: &Context<'_>,
    ) -> Answer {
        if self
            .subtree
            .is_some_and(|device| hex::encode(device) != self.device)
        {
            return Answer::Deny;
        }
        let owner = Authority::person(
            Party::new(vec![Grantee::person(self.device.clone())], []).owning(TreePath::root()),
        )
        .decide(node, kind, cx)
        .answer;
        let Some(invoker) = invoker else {
            return owner;
        };
        let acting = invoker
            .clone()
            .signed_by(&Acting::person(self.device.clone()));
        let policy = WorkspacePolicy::new(self.store, self.device.clone());
        let approved = tilo_action_policy::roles::signed_for(&acting, self.store);
        let as_invoker = Authority::of_acting(&acting, &policy, &approved)
            .decide(node, kind, cx)
            .answer;
        owner.min(as_invoker)
    }
}

#[cfg(test)]
mod own_store_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ReadDoc as _;
    use collab_types::CommitNamed as _;
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{DeviceCapabilities, RevocationReason, sign_device_cert};

    fn device(
        root: &SigningKey,
        n: u8,
        capabilities: DeviceCapabilities,
    ) -> (SigningKey, DeviceCert) {
        let key = SigningKey::from_bytes(&[n; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.device_public_key(),
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
        tilo_clock::wall()
    }

    /// A resource with `field` set to `value`, as an app's first write of it.
    fn resource(field: &str, value: &str) -> DocumentMutation {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, field, value).unwrap();
        doc.commit_edit("tube", field);
        DocumentMutation::Snapshot(doc.save())
    }

    /// Changes that depend on no other write of the resource.
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

    tilo_test_env::scenario! {
        fn a_persons_records_are_written_and_held_only_by_their_current_devices;

        phase a_record_written_on_one_device_is_read_on_another_of_the_same_identity;
        {
            let root = SigningKey::from_bytes(&[1; 32]);
            let root_key = root.verifying_key().to_bytes();
            let (a_key, a_cert) = device(&root, 2, DeviceCapabilities::member());
            let (b_key, b_cert) = device(&root, 3, DeviceCapabilities::member());
            let mut a = PersonalStore::open(root_key, a_cert, now()).unwrap();
            let mut b = PersonalStore::open(root_key, b_cert, now()).unwrap();

            a.publish("tube", "prefs", &resource("theme", "dark"), &a_key, now())
                .unwrap();
            // A hands its documents to no device it holds no certificate of:
            // until B says who it is, A tells it who A is and nothing else.
            let unmet = a
                .catch_up(&a_key, b_key.verifying_key().to_bytes(), now())
                .unwrap();
            assert!(
                unmet
                    .iter()
                    .all(|message| matches!(message, NeighbourMsg::IdentityHello { .. })),
                "a device never met is handed no document: {unmet:?}"
            );
            a.receive(&a_key, b_key.verifying_key().to_bytes(), b.hello(), now())
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

            // B's own write reaches A, and both hold one document.
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

        phase a_device_of_another_identity_writes_nothing_and_holds_nothing;
        {
            let root = SigningKey::from_bytes(&[1; 32]);
            let other = SigningKey::from_bytes(&[9; 32]);
            let (a_key, a_cert) = device(&root, 2, DeviceCapabilities::member());
            let (other_key, other_cert) = device(&other, 10, DeviceCapabilities::member());
            let mut a = PersonalStore::open(root.verifying_key().to_bytes(), a_cert, now()).unwrap();
            let mut stranger =
                PersonalStore::open(other.verifying_key().to_bytes(), other_cert.clone(), now())
                    .unwrap();
            a.publish("tube", "prefs", &resource("theme", "dark"), &a_key, now())
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

            // None of A's documents is among the frames it is given, the
            // certificate it showed refused or not.
            assert!(
                a.catch_up(&a_key, other_key.verifying_key().to_bytes(), now())
                    .unwrap()
                    .iter()
                    .all(|message| matches!(message, NeighbourMsg::IdentityHello { .. }))
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

        phase a_revoked_device_lets_go_of_the_documents_it_held;
        {
            let root = SigningKey::from_bytes(&[1; 32]);
            let root_key = root.verifying_key().to_bytes();
            let (a_key, a_cert) = device(&root, 2, DeviceCapabilities::member());
            let (b_key, b_cert) = device(&root, 3, DeviceCapabilities::member());
            let mut a = PersonalStore::open(root_key, a_cert, now()).unwrap();
            let mut b = PersonalStore::open(root_key, b_cert.clone(), now()).unwrap();
            a.publish("tube", "prefs", &resource("theme", "dark"), &a_key, now())
                .unwrap();
            a.receive(&a_key, b_key.verifying_key().to_bytes(), b.hello(), now())
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

        phase a_replayed_write_changes_nothing;
        {
            let root = SigningKey::from_bytes(&[1; 32]);
            let (a_key, a_cert) = device(&root, 2, DeviceCapabilities::member());
            let mut a = PersonalStore::open(root.verifying_key().to_bytes(), a_cert, now()).unwrap();
            let published = a
                .publish("tube", "prefs", &resource("theme", "dark"), &a_key, now())
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

        phase the_store_survives_being_kept_beside_the_workspaces;
        {
            let root = SigningKey::from_bytes(&[1; 32]);
            let root_key = root.verifying_key().to_bytes();
            let (a_key, a_cert) = device(&root, 2, DeviceCapabilities::member());
            let (_, b_cert) = device(&root, 3, DeviceCapabilities::member());
            let mut a = PersonalStore::open(root_key, a_cert.clone(), now()).unwrap();
            a.publish("tube", "prefs", &resource("theme", "dark"), &a_key, now())
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

        phase a_refused_write_that_later_lands_stops_being_missing;
        // A verdict on a change that later lands is let go, and its author is
        // told so: the write arrived after all, and what this device refused is
        // not missing from it.
        {
            let root = SigningKey::from_bytes(&[12; 32]);
            let (a_key, a_cert) = device(&root, 2, DeviceCapabilities::member());
            let (b_key, _b_cert) = device(&root, 3, DeviceCapabilities::member());
            let mut a = PersonalStore::open(root.verifying_key().to_bytes(), a_cert, now()).unwrap();
            let refused = write(&change("font", "serif"), &b_key, 1, &a.store_key());
            a.file_refusal(
                "tube",
                "prefs",
                &refused,
                &AdmissionError::rejected(
                    RefusalReason::MissingDependency,
                    "it waits for earlier writes",
                ),
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
    }

    #[test]
    fn a_revoked_device_writes_nothing_after_its_revocation() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let (a_key, a_cert) = device(&root, 2, DeviceCapabilities::member());
        let (b_key, b_cert) = device(&root, 3, DeviceCapabilities::member());
        let (c_key, c_cert) = device(&root, 5, DeviceCapabilities::member());
        let mut a = PersonalStore::open(root.verifying_key().to_bytes(), a_cert, now()).unwrap();
        let mut b = PersonalStore::open(root.verifying_key().to_bytes(), b_cert, now()).unwrap();
        a.publish("tube", "prefs", &resource("theme", "dark"), &a_key, now())
            .unwrap();
        // B introduces itself and catches up, and a write of its is admitted
        // before the revocation.
        a.receive(&a_key, b_key.verifying_key().to_bytes(), b.hello(), now())
            .unwrap();
        for message in a
            .catch_up(&a_key, b_key.verifying_key().to_bytes(), now())
            .unwrap()
        {
            b.receive(&b_key, a_key.verifying_key().to_bytes(), message, now())
                .unwrap();
        }
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

    /// The write `mutation` makes of `resource`, as `author`'s own store signs
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

    /// The certificate `key` holds under `root`, for a device that already
    /// sent its own.
    fn b_cert_of(root: &SigningKey, key: &dyn DeviceSigner) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.device_public_key(),
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

    /// A device `seed` names, certified by `signer` for the identity `root`
    /// from `issued_at_ns` until `expires_at_ns`, called `label`.
    fn certificate(
        signer: &SigningKey,
        root: [u8; 32],
        seed: [u8; 32],
        label: &str,
        issued_at_ns: u64,
        expires_at_ns: Option<u64>,
    ) -> (SigningKey, DeviceCert) {
        let key = SigningKey::from_bytes(&seed);
        let mut cert = DeviceCert {
            user_public_key: root,
            device_public_key: key.verifying_key().to_bytes(),
            device_label: label.into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns,
            expires_at_ns,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, signer).unwrap();
        (key, cert)
    }

    /// The resource of the person's devices listing `certificates` as they
    /// are, as a device holding them would write it.
    fn devices_document(certificates: &[DeviceCert]) -> DocumentMutation {
        let mut doc = AutoCommit::new();
        for certificate in certificates {
            doc.put(
                automerge::ROOT,
                hex::encode(certificate.device_public_key),
                automerge::ScalarValue::Bytes(postcard::to_allocvec(certificate).unwrap()),
            )
            .unwrap();
        }
        doc.commit_edit(DEVICES_KIND, "devices");
        DocumentMutation::Snapshot(doc.save())
    }

    /// Two devices of `root` that met: each holds the other's certificate.
    fn met(root: &SigningKey) -> ((SigningKey, PersonalStore), (SigningKey, PersonalStore)) {
        let root_key = root.verifying_key().to_bytes();
        let (a_key, a_cert) = device(root, 2, DeviceCapabilities::member());
        let (b_key, b_cert) = device(root, 3, DeviceCapabilities::member());
        let mut a = PersonalStore::open(root_key, a_cert, now()).unwrap();
        let mut b = PersonalStore::open(root_key, b_cert, now()).unwrap();
        a.receive(&a_key, b_key.verifying_key().to_bytes(), b.hello(), now())
            .unwrap();
        b.receive(&b_key, a_key.verifying_key().to_bytes(), a.hello(), now())
            .unwrap();
        ((a_key, a), (b_key, b))
    }

    fn holds(store: &PersonalStore, device: &DeviceCert) -> bool {
        store
            .devices()
            .iter()
            .any(|held| held.device_public_key == device.device_public_key)
    }

    /// `author` writes the resource of the person's devices as `mutation` and
    /// `reader`, which holds its certificate, receives the write.
    fn hand_on(
        author_key: &SigningKey,
        author: &mut PersonalStore,
        reader_key: &SigningKey,
        reader: &mut PersonalStore,
        mutation: &DocumentMutation,
    ) -> Result<ReceiveOutcome, String> {
        let route = PersonalStore::devices_route();
        let frame = author
            .publish(&route.app, &route.record, mutation, author_key, now())
            .unwrap();
        reader.receive(
            reader_key,
            author_key.verifying_key().to_bytes(),
            frame,
            now(),
        )
    }

    tilo_test_env::scenario! {
        fn a_persons_devices_are_learned_only_from_certificates_that_stand;

        phase a_passed_on_certificate_is_admitted_only_as_a_shown_one_would_be;
        // A certificate one of the person's devices passes on is admitted as
        // the one a device shows for itself is, and only then: another root's,
        // one that root never signed, an expired one and a revoked device's all
        // stay out, and the device they name is unknown here.
        {
            let root = SigningKey::from_bytes(&[1; 32]);
            let root_key = root.verifying_key().to_bytes();
            let other = SigningKey::from_bytes(&[9; 32]);
            let ((a_key, mut a), (b_key, mut b)) = met(&root);

            let (_, sibling) = certificate(&root, root_key, [5; 32], "sibling", 1, None);
            let (_, anothers) = certificate(
                &other,
                other.verifying_key().to_bytes(),
                [6; 32],
                "another person's",
                1,
                None,
            );
            // Names this identity, signed by a key that is not its root.
            let (_, mut forged) = certificate(
                &other,
                other.verifying_key().to_bytes(),
                [7; 32],
                "forged",
                1,
                None,
            );
            forged.user_public_key = root_key;
            let (_, expired) = certificate(&root, root_key, [8; 32], "expired", 1, Some(2));
            let (gone_key, gone) = certificate(&root, root_key, [10; 32], "revoked", 1, None);
            // A already holds the revocation its identity signed.
            assert!(a.revoke(revocation(&root, &gone_key)).unwrap());

            let outcome = hand_on(
                &b_key,
                &mut b,
                &a_key,
                &mut a,
                &devices_document(&[
                    sibling.clone(),
                    anothers.clone(),
                    forged.clone(),
                    expired.clone(),
                    gone.clone(),
                ]),
            )
            .unwrap();
            assert!(outcome.metadata_changed, "a device was learned");
            assert!(holds(&a, &sibling), "the root's current certificate");
            for (refused, why) in [
                (&anothers, "another root's"),
                (&forged, "not the root's signature"),
                (&expired, "expired"),
                (&gone, "revoked"),
            ] {
                assert!(!holds(&a, refused), "{why} certificate is not admitted");
                assert!(!a.holds_device(&refused.device_public_key, now()), "{why}");
            }
            assert_eq!(
                a.devices().len(),
                3,
                "this device, the one met, the one learned"
            );
            // Nobody is told a write of theirs was refused: the resource is the
            // store's own, and an app's listing never shows it.
            assert!(a.write_refusals().is_empty());
            assert!(a.rows().is_empty());
            assert!(b.rows().is_empty());
        }

        phase a_revocation_wins_over_a_certificate_that_was_passed_on;
        // A device learned from the resource is one its person revoked as soon as
        // the revocation arrives: it is no device of theirs here from then on,
        // and listing it again changes nothing.
        {
            let root = SigningKey::from_bytes(&[1; 32]);
            let root_key = root.verifying_key().to_bytes();
            let ((a_key, mut a), (b_key, mut b)) = met(&root);
            let (gone_key, gone) = certificate(&root, root_key, [10; 32], "lost", 1, None);
            hand_on(
                &b_key,
                &mut b,
                &a_key,
                &mut a,
                &devices_document(std::slice::from_ref(&gone)),
            )
            .unwrap();
            assert!(a.holds_device(&gone.device_public_key, now()));

            // The revocation reaches A as it always did: in a sibling's hello.
            let outcome = a
                .receive(
                    &a_key,
                    b_key.verifying_key().to_bytes(),
                    hello(b.device(), vec![revocation(&root, &gone_key)]),
                    now(),
                )
                .unwrap();
            assert!(outcome.metadata_changed);
            assert!(!a.holds_device(&gone.device_public_key, now()));
            assert!(a.admits_device(&gone.device_public_key, now()).is_err());

            // Listed again, with a later certificate even, it stays out.
            let (_, later) = certificate(&root, root_key, [10; 32], "lost", 5, None);
            hand_on(&b_key, &mut b, &a_key, &mut a, &devices_document(&[later])).unwrap();
            assert!(!a.holds_device(&gone.device_public_key, now()));
            assert!(a.admits_device(&gone.device_public_key, now()).is_err());
            // And A lists it to no one: what it adds to the resource leaves it out.
            assert!(
                !a.unlisted_devices(now())
                    .iter()
                    .any(|listed| listed.device_public_key == gone.device_public_key)
            );
        }

        phase a_later_certificate_replaces_an_earlier_one_and_never_the_reverse;
        // A device's later certificate replaces the earlier one held for it,
        // and an earlier one listed afterwards does not come back: two devices
        // holding different certificates of a third settle on the later.
        {
            let root = SigningKey::from_bytes(&[1; 32]);
            let root_key = root.verifying_key().to_bytes();
            let ((a_key, mut a), (b_key, mut b)) = met(&root);
            let (_, earlier) = certificate(&root, root_key, [5; 32], "phone", 1, None);
            let (_, later) = certificate(&root, root_key, [5; 32], "renamed phone", 7, None);
            let label = |store: &PersonalStore| {
                store
                    .devices()
                    .into_iter()
                    .find(|held| held.device_public_key == earlier.device_public_key)
                    .map(|held| held.device_label)
            };

            hand_on(
                &b_key,
                &mut b,
                &a_key,
                &mut a,
                &devices_document(std::slice::from_ref(&earlier)),
            )
            .unwrap();
            assert_eq!(label(&a).as_deref(), Some("phone"));
            hand_on(
                &b_key,
                &mut b,
                &a_key,
                &mut a,
                &devices_document(std::slice::from_ref(&later)),
            )
            .unwrap();
            assert_eq!(label(&a).as_deref(), Some("renamed phone"));
            hand_on(
                &b_key,
                &mut b,
                &a_key,
                &mut a,
                &devices_document(std::slice::from_ref(&earlier)),
            )
            .unwrap();
            assert_eq!(label(&a).as_deref(), Some("renamed phone"));
            // What A adds to the resource is the later one, once: it settles.
            a.take_devices_update(&a_key, now()).unwrap();
            assert!(a.take_devices_update(&a_key, now()).unwrap().is_none());
            assert!(a.unlisted_devices(now()).is_empty());
        }

        phase no_more_devices_are_taken_from_the_record_than_it_may_name;
        // The resource names at most [`MAX_DEVICES`] devices to a device: entries
        // past that are not read, and a device adds none to a full resource.
        {
            let root = SigningKey::from_bytes(&[1; 32]);
            let root_key = root.verifying_key().to_bytes();
            let ((a_key, mut a), (b_key, mut b)) = met(&root);
            let many: Vec<DeviceCert> = (0..MAX_DEVICES + 9)
                .map(|n| {
                    let mut seed = [0x40; 32];
                    seed[..8].copy_from_slice(&u64::try_from(n).unwrap().to_le_bytes());
                    certificate(&root, root_key, seed, "one of many", 1, None).1
                })
                .collect();
            hand_on(&b_key, &mut b, &a_key, &mut a, &devices_document(&many)).unwrap();
            assert_eq!(
                a.devices().len(),
                MAX_DEVICES + 2,
                "this device, the one met, and no more than the resource may name"
            );
            // The resource is full: A adds neither itself nor the device it met.
            let entries = |store: &PersonalStore| {
                let held = store
                    .documents
                    .get(&PersonalStore::devices_route())
                    .unwrap();
                held.doc.length(automerge::ROOT)
            };
            assert_eq!(entries(&a), MAX_DEVICES + 9);
            assert!(a.unlisted_devices(now()).is_empty());
            a.take_devices_update(&a_key, now()).unwrap();
            assert_eq!(entries(&a), MAX_DEVICES + 9);
        }

        phase a_device_of_another_person_is_told_of_no_device_and_teaches_none;
        // A device of another person is handed the resource of none of the
        // person's devices, at the start of a link or while it lasts, and
        // nothing it lists is admitted: who the person's devices are is said
        // only between them.
        {
            let root = SigningKey::from_bytes(&[1; 32]);
            let root_key = root.verifying_key().to_bytes();
            let other = SigningKey::from_bytes(&[9; 32]);
            let ((a_key, mut a), (b_key, _b)) = met(&root);
            let (stranger_key, stranger_cert) = device(&other, 10, DeviceCapabilities::member());
            let stranger = stranger_key.verifying_key().to_bytes();
            let devices = |message: &NeighbourMsg| {
                matches!(message, NeighbourMsg::AppDocument { app_id, document_id, .. }
                    if PersonalStore::is_devices_document(app_id, document_id))
            };

            // A's resource lists itself and the device it met.
            let published = a.take_devices_update(&a_key, now()).unwrap().unwrap();
            assert!(devices(&published));
            // One of the person's devices is handed it, live and catching up.
            let sibling = b_key.verifying_key().to_bytes();
            assert!(a.hands(&published, &sibling, now()));
            assert!(
                a.catch_up(&a_key, sibling, now())
                    .unwrap()
                    .iter()
                    .any(devices)
            );
            // A device that showed nothing, and one of another person, is not.
            assert!(!a.hands(&published, &stranger, now()));
            assert!(
                !a.catch_up(&a_key, stranger, now())
                    .unwrap()
                    .iter()
                    .any(devices)
            );
            assert!(
                a.receive(&a_key, stranger, hello(&stranger_cert, Vec::new()), now())
                    .is_err()
            );
            assert!(!a.hands(&published, &stranger, now()));
            assert!(
                !a.catch_up(&a_key, stranger, now())
                    .unwrap()
                    .iter()
                    .any(devices)
            );

            // What it lists is refused with the write that carries it, even a
            // certificate the person's root did sign, and nobody keeps a verdict.
            let (_, sibling_cert) = certificate(&root, root_key, [5; 32], "sibling", 1, None);
            let route = PersonalStore::devices_route();
            let envelope = devices_write(
                &devices_document(&[sibling_cert.clone(), stranger_cert.clone()]),
                &stranger_key,
                &a.store_key(),
            );
            let refused = a.receive(
                &a_key,
                stranger,
                NeighbourMsg::AppDocument {
                    peer: peer_handle(&stranger),
                    app_id: route.app,
                    cluster_id: a.store_key(),
                    document_id: route.record,
                    envelope: Box::new(envelope),
                    readers: None,
                },
                now(),
            );
            assert!(refused.is_err());
            assert!(!holds(&a, &sibling_cert));
            assert!(!holds(&a, &stranger_cert));
            assert_eq!(a.devices().len(), 2);
            assert!(a.write_refusals().is_empty());
        }

        phase a_store_opens_after_a_siblings_certificate_expired;
        // A store kept while it held a sibling's certificate opens after that
        // certificate expired: the person's documents are there, and the device
        // whose certificate no longer holds is not one of theirs here.
        {
            let root = SigningKey::from_bytes(&[1; 32]);
            let root_key = root.verifying_key().to_bytes();
            let (a_key, a_cert) = device(&root, 2, DeviceCapabilities::member());
            let kept_at = now();
            let expires = now_ns(kept_at + Duration::from_secs(60));
            let (_, short) = certificate(&root, root_key, [5; 32], "short-lived", 1, Some(expires));
            let mut a = PersonalStore::open(root_key, a_cert.clone(), kept_at).unwrap();
            assert!(a.admit_device(&short, kept_at).unwrap());
            a.publish("tube", "prefs", &resource("theme", "dark"), &a_key, kept_at)
                .unwrap();
            let kept = a.durable().unwrap();

            let later = kept_at + Duration::from_secs(120);
            let restored = PersonalStore::restore(&kept, &a_cert, later).unwrap();
            assert_eq!(
                field(&restored.document("tube", "prefs").unwrap(), "theme").as_deref(),
                Some("dark")
            );
            assert!(!holds(&restored, &short));
            assert!(!restored.holds_device(&short.device_public_key, later));
        }

        phase a_document_is_handed_only_to_a_device_that_is_the_persons_now;
        // The person's documents, as snapshots when a link starts and as writes
        // while it lasts, are handed to a device only while this store holds a
        // certificate that makes it one of the person's: not to one that showed
        // none, one of another person's, a revoked one, or one whose certificate
        // ran out. A refusal names its resource to its author and to no stranger.
        {
            let root = SigningKey::from_bytes(&[1; 32]);
            let root_key = root.verifying_key().to_bytes();
            let other = SigningKey::from_bytes(&[9; 32]);
            let (a_key, a_cert) = device(&root, 2, DeviceCapabilities::member());
            let mut a = PersonalStore::open(root_key, a_cert, now()).unwrap();
            let written = a
                .publish("tube", "prefs", &resource("theme", "dark"), &a_key, now())
                .unwrap();
            // Whether `device` is handed a document at `at`: in what it catches
            // up from, and of what is written while its link lasts.
            let handed = |a: &mut PersonalStore, device: [u8; 32], at: SystemTime| {
                let caught_up = a
                    .catch_up(&a_key, device, at)
                    .unwrap()
                    .iter()
                    .any(|message| matches!(message, NeighbourMsg::AppDocument { .. }));
                (caught_up, a.hands(&written, &device, at))
            };

            // A device that has shown nothing.
            let (new_key, new_cert) = certificate(&root, root_key, [5; 32], "new", 1, None);
            let new = new_key.verifying_key().to_bytes();
            assert_eq!(handed(&mut a, new, now()), (false, false));

            // A device of another person, before and after the certificate it
            // shows is refused.
            let (stranger_key, stranger_cert) = device(&other, 10, DeviceCapabilities::member());
            let stranger = stranger_key.verifying_key().to_bytes();
            assert_eq!(handed(&mut a, stranger, now()), (false, false));
            assert!(
                a.receive(&a_key, stranger, hello(&stranger_cert, Vec::new()), now())
                    .is_err()
            );
            assert_eq!(handed(&mut a, stranger, now()), (false, false));

            // The person's new device, once the certificate it shows is admitted.
            a.receive(&a_key, new, hello(&new_cert, Vec::new()), now())
                .unwrap();
            assert_eq!(handed(&mut a, new, now()), (true, true));

            // The same device, once the person took it away.
            assert!(a.revoke(revocation(&root, &new_key)).unwrap());
            assert_eq!(handed(&mut a, new, now()), (false, false));

            // A device whose certificate holds, and then has run out.
            let at = now();
            let (short_key, short_cert) = certificate(
                &root,
                root_key,
                [6; 32],
                "short-lived",
                1,
                Some(now_ns(at + Duration::from_secs(60))),
            );
            let short = short_key.verifying_key().to_bytes();
            a.receive(&a_key, short, hello(&short_cert, Vec::new()), at)
                .unwrap();
            assert_eq!(handed(&mut a, short, at), (true, true));
            // It holds through its last nanosecond, and has run out at its end.
            let ends = at + Duration::from_secs(60);
            let last = ends.checked_sub(Duration::from_nanos(1)).unwrap();
            assert_eq!(handed(&mut a, short, last), (true, true));
            assert_eq!(handed(&mut a, short, ends), (false, false));
            let later = at + Duration::from_secs(120);
            assert_eq!(handed(&mut a, short, later), (false, false));

            // What A refuses of that device's writes names the record: its
            // author is told, whatever became of its certificate, and a device
            // that is not the person's is not.
            let refused = write(&change("font", "serif"), &short_key, 1, &a.store_key());
            a.file_refusal(
                "tube",
                "prefs",
                &refused,
                &AdmissionError::rejected(
                    RefusalReason::MissingDependency,
                    "it waits for earlier writes",
                ),
                &a_key,
                now(),
            )
            .unwrap();
            let told = a.take_refusal_updates();
            assert_eq!(told.len(), 1, "one set, about the one author: {told:?}");
            assert!(a.hands(&told[0], &short, later));
            assert!(!a.hands(&told[0], &stranger, now()));
        }
    }

    /// The op `author` signs for `mutation` of the resource of the person's
    /// devices in the store keyed `store_key`.
    fn devices_write(
        mutation: &DocumentMutation,
        author: &SigningKey,
        store_key: &str,
    ) -> SignedOp {
        let route = PersonalStore::devices_route();
        let mut envelope = SignedOp::new_unsigned(
            mutation.encode().unwrap(),
            collab_types::document_artifact_id(&route.app, store_key, &route.record),
            Ed25519PublicKey::from_bytes(author.verifying_key().to_bytes()),
            collab_types::document_attestation_id(&route.app, store_key, &route.record),
            OpKind::Change,
            now(),
            1,
        );
        collab_types::sign_op(&mut envelope, author).unwrap();
        envelope
    }
}
