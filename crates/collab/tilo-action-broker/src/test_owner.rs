//! A durable owner of one room its own device governs, behind a bounded
//! commit mailbox: the owner-commit boundary the broker's tests cross.
//!
//! Each applied commit admits the signed document (or records the effect
//! receipt in the room) on a candidate peer, writes the candidate and its
//! generation to `session.bin`, and only then replaces the live peer and
//! answers. A failed write leaves the live owner unchanged.
#![allow(
    clippy::missing_panics_doc,
    reason = "test fixture: a failed step is the test failing"
)]
use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
};
use tilo_owner_commit::{
    CommitFuture, CommitOperation, OwnerCommitReceipt, OwnerCommitRequest, OwnerCommitService,
};
use tilo_peer_state::{SeedDocument, SeedState, StatePeer, TransportOrigin};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use tokio::sync::{mpsc, oneshot};

const STATE: &str = "session.bin";
/// The seeded Writer record.
pub const WRITER: (&str, &str) = ("so.tilo.word-processor", "tab:0000000000000001");

#[derive(Serialize, Deserialize)]
struct Stored {
    person: [u8; 32],
    key: [u8; 32],
    authority: DeviceCert,
    room: String,
    peer: Vec<u8>,
    generation: u64,
}

/// What the owner has durably committed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub room: String,
    pub generation: u64,
    pub aggregate: [u8; 32],
}

pub struct SessionOwner {
    root: PathBuf,
    person: SigningKey,
    key: SigningKey,
    peer: StatePeer,
    generation: u64,
    outgoing: Vec<Vec<u8>>,
}

/// The Writer record as a record store reserves it, carrying its identity.
fn writer_seed(room: &str) -> Result<Vec<u8>, String> {
    use ui::record::{RecordCheckout, RecordId, RecordOwners};
    let definition = word_processor::definition();
    let checkout = RecordCheckout::new(RecordId::for_tab(1), definition.record_kind.clone());
    let mut owners = RecordOwners::portable(ui::portable_storage::PortableStorage::new(
        "test-owner-initialize",
        room,
    )?);
    let records = owners
        .open(definition.app_id, room)
        .map_err(|e| e.to_string())?;
    records
        .reserve_record(&checkout)
        .map_err(|e| e.to_string())?
        .initialize(word_processor::initialize_empty_document)
        .map_err(|e| e.to_string())?;
    records
        .read(&checkout.id, |doc| doc.clone().save())
        .ok_or_else(|| "writer seed missing".to_string())
}

impl SessionOwner {
    /// A new room under `dir`, governed by a fresh device with a Writer record.
    pub fn initialize(dir: &Path) -> Result<Self, String> {
        Self::initialize_with_documents(dir, |_| Ok(Vec::new()))
    }

    /// Add real typed record genesis to the same durable owner fixture.
    pub fn initialize_with_documents(
        dir: &Path,
        documents: impl FnOnce(&str) -> Result<Vec<SeedDocument>, String>,
    ) -> Result<Self, String> {
        Self::initialize_in_room(dir, &uuid::Uuid::new_v4().to_string(), documents)
    }

    /// Keep multiple real owners in the same authenticated fixture room.
    pub fn initialize_in_room(
        dir: &Path,
        room: &str,
        documents: impl FnOnce(&str) -> Result<Vec<SeedDocument>, String>,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let seed: Vec<u8> = (0..4)
            .flat_map(|_| *uuid::Uuid::new_v4().as_bytes())
            .collect();
        let person = SigningKey::from_bytes(seed[..32].try_into().unwrap());
        let key = SigningKey::from_bytes(seed[32..].try_into().unwrap());
        let mut authority = DeviceCert {
            user_public_key: person.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "Governing device".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut authority, &person).map_err(|e| e.to_string())?;
        let room = room.to_owned();
        let now = SystemTime::now();
        let mut peer = StatePeer::new(
            &room,
            authority.clone(),
            vec![authority.clone()],
            RevocationList::new(),
            now,
        )?;
        let writer = writer_seed(&room)?;
        let mut seeded = documents(&room)?;
        seeded.push(SeedDocument {
            app_id: WRITER.0.into(),
            record_id: WRITER.1.into(),
            kind: word_processor::definition().record_kind.as_str().into(),
            bytes: writer,
        });
        peer.install_seed(
            SeedState {
                room_changes: tilo_peer_state::seed_room(
                    &room,
                    &authority,
                    std::slice::from_ref(&authority),
                )?,
                documents: seeded,
            },
            &key,
            now,
        )?;
        let mut owner = Self {
            root: dir.to_path_buf(),
            person,
            key,
            peer,
            generation: 0,
            outgoing: Vec::new(),
        };
        let peer = owner.peer.clone();
        owner.commit(peer)?;
        Ok(owner)
    }

    /// The key of this owner's person's [`Self::second_device`].
    pub fn second_device_key() -> SigningKey {
        SigningKey::from_bytes(&[90; 32])
    }

    /// A second device of this owner's person, certified by them: the same
    /// root and room, a different device.
    pub fn second_device(&self) -> DeviceCert {
        let mut certificate = DeviceCert {
            user_public_key: self.person.verifying_key().to_bytes(),
            device_public_key: Self::second_device_key().verifying_key().to_bytes(),
            device_label: "Second device".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut certificate, &self.person).unwrap();
        certificate
    }

    /// Admit [`Self::second_device`] into this owner's room, as the
    /// governing device, and commit the room that admits it.
    pub fn admit_second_device(&mut self) -> Result<(), String> {
        let certificate = self.second_device();
        let now = SystemTime::now();
        let mut candidate = self.peer.clone();
        candidate.admit_certificate(certificate, &self.key, now)?;
        self.commit(candidate)
    }

    /// The owner `root` last committed.
    pub fn open(root: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(root.join(STATE)).map_err(|e| e.to_string())?;
        let stored: Stored = postcard::from_bytes(&bytes).map_err(|e| e.to_string())?;
        let peer = StatePeer::restore(
            &stored.peer,
            &stored.room,
            stored.authority.clone(),
            vec![stored.authority],
            RevocationList::new(),
            SystemTime::now(),
        )?;
        let mut owner = Self {
            root: root.to_path_buf(),
            person: SigningKey::from_bytes(&stored.person),
            key: SigningKey::from_bytes(&stored.key),
            peer,
            generation: stored.generation,
            outgoing: Vec::new(),
        };
        owner.outgoing = owner.publications(owner.peer.clone())?;
        Ok(owner)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn peer(&self) -> &StatePeer {
        &self.peer
    }
    /// The governing person's root key.
    pub fn person_key(&self) -> &SigningKey {
        &self.person
    }
    /// The governing device's key.
    pub fn signing_key(&self) -> &SigningKey {
        &self.key
    }
    pub fn status(&self) -> Status {
        Status {
            room: self.peer.room().into(),
            generation: self.generation,
            aggregate: *blake3::hash(&self.peer.durable().unwrap()).as_bytes(),
        }
    }
    /// The room's current signed checkpoint, for a replica to converge on.
    pub fn outgoing(&self) -> &[Vec<u8>] {
        &self.outgoing
    }

    fn publications(&self, mut peer: StatePeer) -> Result<Vec<Vec<u8>>, String> {
        peer.publication_mut()
            .durable_bootstrap_messages(&self.key, SystemTime::now(), self.generation.max(1))?
            .iter()
            .map(|message| postcard::to_stdvec(message).map_err(|e| e.to_string()))
            .collect()
    }

    fn commit(&mut self, peer: StatePeer) -> Result<(), String> {
        let generation = self
            .generation
            .checked_add(1)
            .ok_or("generation exhausted")?;
        let bytes = postcard::to_stdvec(&Stored {
            person: self.person.to_bytes(),
            key: self.key.to_bytes(),
            authority: peer.authority().clone(),
            room: peer.room().into(),
            peer: peer.durable()?,
            generation,
        })
        .map_err(|e| e.to_string())?;
        let path = self.root.join(STATE);
        let staged = path.with_extension("staged");
        std::fs::write(&staged, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&staged, &path).map_err(|e| e.to_string())?;
        self.generation = generation;
        self.outgoing = self.publications(peer.clone())?;
        self.peer = peer;
        Ok(())
    }

    fn apply(&mut self, request: &OwnerCommitRequest) -> Result<OwnerCommitReceipt, String> {
        let binding = request.binding()?;
        if request.room != self.peer.room() {
            return Err("owner namespace unavailable".into());
        }
        let now = SystemTime::now();
        let mut candidate = self.peer.clone();
        match &request.operation {
            CommitOperation::Document {
                app,
                record,
                envelope,
            } => {
                let outcome =
                    candidate.admit_local_app_document(app, record, *envelope.clone(), now)?;
                if outcome.deferred || outcome.incomplete.is_some() {
                    return Err("local publication admission incomplete".into());
                }
            }
            CommitOperation::EffectReceipt {
                invocation,
                redacted,
            } => {
                let receipt: tilo_desktop_model::WorkspaceEffectReceipt =
                    serde_json::from_slice(redacted).map_err(|e| e.to_string())?;
                if receipt.invocation.0 != *invocation {
                    return Err("effect receipt invocation mismatch".into());
                }
                let mut doc = candidate
                    .admitted_room()
                    .ok_or("owner governance missing")?
                    .clone();
                doc.set_actor(automerge::ActorId::from(
                    self.key.verifying_key().to_bytes().to_vec(),
                ));
                let object = match doc
                    .get(ROOT, "effect_receipts_v1")
                    .map_err(|e| e.to_string())?
                {
                    Some((Value::Object(ObjType::Map), object)) => object,
                    Some(_) => return Err("invalid canonical effect receipt map".into()),
                    None => doc
                        .put_object(ROOT, "effect_receipts_v1", ObjType::Map)
                        .map_err(|e| e.to_string())?,
                };
                let id = invocation.to_string();
                match doc.get(&object, &id).map_err(|e| e.to_string())? {
                    Some((Value::Scalar(value), _)) if matches!(value.as_ref(), ScalarValue::Bytes(prior) if prior == redacted) =>
                    {
                        return Ok(OwnerCommitReceipt {
                            binding,
                            generation: self.generation,
                            aggregate_hash: self.status().aggregate,
                        });
                    }
                    Some(_) => return Err("effect receipt is immutable".into()),
                    None => {
                        doc.put(&object, &id, ScalarValue::Bytes(redacted.clone()))
                            .map_err(|e| e.to_string())?;
                    }
                }
                doc.commit();
                let changes = doc
                    .get_changes(&[])
                    .into_iter()
                    .map(|change| change.raw_bytes().to_vec())
                    .collect();
                candidate.receive_from(
                    TransportOrigin::RelayDelta(self.key.verifying_key().to_bytes()),
                    NeighbourMsg::SyncResp {
                        room: request.room.clone(),
                        changes,
                    },
                    now,
                )?;
            }
            _ => return Err("this owner commits documents and effect receipts only".into()),
        }
        self.commit(candidate)?;
        Ok(OwnerCommitReceipt {
            binding,
            generation: self.generation,
            aggregate_hash: self.status().aggregate,
        })
    }
}

pub mod owner_commit {
    use super::*;

    /// The broker's side of the mailbox. A commit waits for the owner's turn;
    /// a turn that never answers leaves the outcome unknown and the handle
    /// unhealthy.
    #[derive(Clone)]
    pub struct OwnerCommitHandle {
        sender: mpsc::Sender<PendingOwnerCommit>,
        failed: Arc<AtomicBool>,
    }

    pub struct OwnerCommitMailbox {
        receiver: mpsc::Receiver<PendingOwnerCommit>,
    }

    pub struct PendingOwnerCommit {
        request: OwnerCommitRequest,
        reply: oneshot::Sender<Result<OwnerCommitReceipt, String>>,
    }

    impl OwnerCommitMailbox {
        pub fn bounded(capacity: usize) -> (OwnerCommitHandle, Self) {
            let (sender, receiver) = mpsc::channel(capacity.max(1));
            (
                OwnerCommitHandle {
                    sender,
                    failed: Arc::default(),
                },
                Self { receiver },
            )
        }
        pub async fn recv(&mut self) -> Option<PendingOwnerCommit> {
            self.receiver.recv().await
        }
        /// Whether a commit is waiting for the owner's turn now. The broker
        /// hands a commit over before it waits for the owner's answer, so
        /// once a call that would have committed returned, its commit shows.
        pub fn is_empty(&self) -> bool {
            self.receiver.is_empty()
        }
    }

    impl PendingOwnerCommit {
        /// Commit the request in `owner` and answer the waiting broker.
        pub fn apply(self, owner: &mut SessionOwner) -> Result<OwnerCommitReceipt, String> {
            let result = owner.apply(&self.request);
            let _ = self.reply.send(result.clone());
            result
        }
    }

    impl OwnerCommitService for OwnerCommitHandle {
        fn health(&self) -> Result<(), String> {
            if self.failed.load(Ordering::SeqCst) {
                return Err("owner commit failed earlier; outcome unknown".into());
            }
            if self.sender.is_closed() {
                return Err("owner commit mailbox closed; outcome unknown".into());
            }
            Ok(())
        }
        fn commit(&self, request: OwnerCommitRequest) -> CommitFuture {
            let this = self.clone();
            Box::pin(async move {
                this.health()?;
                request.binding()?;
                let (reply, response) = oneshot::channel();
                this.sender
                    .try_send(PendingOwnerCommit {
                        request: request.clone(),
                        reply,
                    })
                    .map_err(|_| {
                        this.failed.store(true, Ordering::SeqCst);
                        "owner commit capacity exhausted; outcome unknown".to_owned()
                    })?;
                let receipt = response.await.map_err(|_| {
                    this.failed.store(true, Ordering::SeqCst);
                    "owner commit reply lost; outcome unknown".to_owned()
                })??;
                receipt.validate_for(&request)?;
                Ok(receipt)
            })
        }
    }
}
