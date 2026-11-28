//! A durable record of one workspace this device hosts, behind a bounded
//! write mailbox: the boundary the broker's tests cross.
//!
//! Each applied write admits the signed document (or records the effect receipt
//! in the workspace) on a candidate peer, writes the candidate and its
//! generation to `session.bin`, and only then replaces the live peer and
//! answers. A failed write leaves the live record unchanged.
#![allow(
    clippy::missing_panics_doc,
    reason = "test fixture: a failed step is the test failing"
)]
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tilo_broker_core::workspace_commits::{EffectFuture, WorkspaceCommits};
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::{SeedDocument, SeedState, StatePeer, TransportOrigin};
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::{mpsc, oneshot};

use tilo_peer_state::fixture::{member_cert, primary_cert};
const STATE: &str = "session.bin";
/// The seeded document record.
pub const WRITER: (&str, &str) = (test_document_app::APP_ID, "tab:0000000000000001");

#[derive(Serialize, Deserialize)]
struct Stored {
    person: [u8; 32],
    key: [u8; 32],
    authority: DeviceCert,
    workspace: String,
    peer: Vec<u8>,
    generation: u64,
}

/// What the fixture's record last committed.
#[allow(
    dead_code,
    reason = "the fixture's surface is what the broker's tests need of it"
)]
pub struct Status {
    pub workspace: String,
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
fn writer_seed(workspace: &str) -> Result<Vec<u8>, String> {
    use tilo_ui_model::resource::{DocumentCheckout, DocumentId, ResourceOwners};
    let definition = test_document_app::definition();
    let checkout = DocumentCheckout::new(DocumentId::for_tab(1), definition.resource_type.clone());
    let mut owners = ResourceOwners::in_memory();
    let records = owners
        .open(definition.app_id(), workspace)
        .map_err(|e| e.to_string())?;
    // The record's type is declared by the app that holds it: what founds the containers its
    // first value fills, alike on every device.
    records.declare_type(
        <test_document_app::Document as tilo_resource::Resource>::KIND,
        <test_document_app::Document as tilo_resource::Resource>::surface,
    );
    records
        .reserve_resource(&checkout)
        .map_err(|e| e.to_string())?
        .initialize(test_document_app::initialize_empty_document)
        .map_err(|e| e.to_string())?;
    records
        .read(&checkout.id, |doc| doc.clone().save())
        .ok_or_else(|| "writer seed missing".to_string())
}

#[allow(
    dead_code,
    reason = "the fixture's surface is what the broker's tests need of it"
)]
impl SessionOwner {
    /// A new workspace under `dir`, hosted by a fresh device with a Writer record.
    pub fn initialize(dir: &Path) -> Result<Self, String> {
        Self::initialize_with_documents(dir, |_| Ok(Vec::new()))
    }

    /// Add real typed record genesis to the same durable owner fixture.
    pub fn initialize_with_documents(
        dir: &Path,
        documents: impl FnOnce(&str) -> Result<Vec<SeedDocument>, String>,
    ) -> Result<Self, String> {
        Self::initialize_in_workspace(dir, &uuid::Uuid::new_v4().to_string(), documents)
    }

    /// Keep multiple real owners in the same authenticated fixture workspace.
    pub fn initialize_in_workspace(
        dir: &Path,
        workspace: &str,
        documents: impl FnOnce(&str) -> Result<Vec<SeedDocument>, String>,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let seed: Vec<u8> = (0..4)
            .flat_map(|_| *uuid::Uuid::new_v4().as_bytes())
            .collect();
        let person = SigningKey::from_bytes(seed[..32].try_into().unwrap());
        let key = SigningKey::from_bytes(seed[32..].try_into().unwrap());
        let authority = primary_cert(&person, &key, "Governing device");
        let workspace = workspace.to_owned();
        let now = tilo_clock::wall();
        let mut peer = StatePeer::new(
            &workspace,
            authority.clone(),
            vec![authority.clone()],
            RevocationList::new(),
            now,
        )?;
        let writer = writer_seed(&workspace)?;
        let mut seeded = documents(&workspace)?;
        seeded.push(SeedDocument {
            app_id: WRITER.0.into(),
            document_id: WRITER.1.into(),
            kind: test_document_app::definition()
                .resource_type
                .as_str()
                .into(),
            bytes: writer,
        });
        peer.install_seed(
            SeedState {
                workspace_changes: tilo_peer_state::seed_workspace(
                    &workspace,
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
    /// root and workspace, a different device.
    pub fn second_device(&self) -> DeviceCert {
        member_cert(&self.person, &Self::second_device_key(), "Second device")
    }

    /// The owner `root` last committed.
    pub fn open(root: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(root.join(STATE)).map_err(|e| e.to_string())?;
        let stored: Stored = postcard::from_bytes(&bytes).map_err(|e| e.to_string())?;
        let peer = StatePeer::restore(
            &stored.peer,
            &stored.workspace,
            stored.authority.clone(),
            vec![stored.authority],
            RevocationList::new(),
            tilo_clock::wall(),
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
    /// The owner's root key.
    pub fn person_key(&self) -> &SigningKey {
        &self.person
    }
    /// This owner device's key.
    pub fn signing_key(&self) -> &SigningKey {
        &self.key
    }
    /// This owner device's certificate.
    pub fn device(&self) -> DeviceCert {
        let device = self.key.verifying_key().to_bytes();
        self.peer
            .owner_certificates()
            .into_iter()
            .find(|cert| cert.device_public_key == device)
            .expect("the owner fixture's device is in its workspace")
    }
    pub fn status(&self) -> Status {
        Status {
            workspace: self.peer.workspace_id().into(),
            generation: self.generation,
            aggregate: *blake3::hash(&self.peer.durable().unwrap()).as_bytes(),
        }
    }
    /// The workspace's current signed checkpoint, for a replica to converge on.
    pub fn outgoing(&self) -> &[Vec<u8>] {
        &self.outgoing
    }

    fn publications(&self, mut peer: StatePeer) -> Result<Vec<Vec<u8>>, String> {
        peer.publication_mut()
            .bootstrap_messages(&self.key, tilo_clock::wall())?
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
            authority: self.device(),
            workspace: peer.workspace_id().into(),
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

    fn apply(&mut self, change: &Change) -> Result<(), String> {
        let now = tilo_clock::wall();
        let mut candidate = self.peer.clone();
        let receipt: tilo_actions::receipt::WorkspaceEffectReceipt =
            serde_json::from_slice(&change.redacted).map_err(|e| e.to_string())?;
        let mut doc = candidate
            .admitted_workspace()
            .ok_or("owner governance missing")?
            .clone();
        doc.set_actor(automerge::ActorId::from(
            self.key.verifying_key().to_bytes().to_vec(),
        ));
        match tilo_desktop_model::projection::workspace_effect_receipts_of(&doc)
            .into_iter()
            .find(|held| held.invocation == receipt.invocation)
        {
            Some(held) if held == receipt => return Ok(()),
            Some(_) => return Err("effect receipt is immutable".into()),
            None => tilo_desktop_model::projection::project_effect_receipts(
                &mut doc,
                &hex::encode(self.key.verifying_key().to_bytes()),
                std::slice::from_ref(&receipt),
            )?,
        }
        doc.commit();
        let changes = doc
            .get_changes(&[])
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        candidate
            .receive_from(
                TransportOrigin::ServerDelta(self.key.verifying_key().to_bytes()),
                NeighbourMsg::SyncResp {
                    changes,
                    workspace: candidate.workspace_id().into(),
                },
                now,
            )
            .map_err(|error| error.to_string())?;
        self.commit(candidate)
    }
}

/// A write the broker hands the record: an effect's terminal receipt.
#[derive(Clone, Debug)]
pub struct Change {
    pub redacted: Vec<u8>,
}

pub mod writes {
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
        change: Change,
        reply: oneshot::Sender<Result<(), String>>,
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
        #[allow(
            dead_code,
            reason = "the fixture's surface is what the broker's tests need of it"
        )]
        pub fn is_empty(&self) -> bool {
            self.receiver.is_empty()
        }
    }

    impl PendingOwnerCommit {
        /// Apply the change in `owner` and answer the waiting broker.
        pub fn apply(self, owner: &mut SessionOwner) -> Result<(), String> {
            let result = owner.apply(&self.change);
            let _ = self.reply.send(result.clone());
            result
        }
    }

    impl WorkspaceCommits for OwnerCommitHandle {
        fn projects_workspace(&self, _: WorkspaceId) -> bool {
            true
        }

        fn health(&self) -> Result<(), String> {
            if self.failed.load(Ordering::SeqCst) {
                return Err("the record failed earlier; outcome unknown".into());
            }
            if self.sender.is_closed() {
                return Err("the record's mailbox closed; outcome unknown".into());
            }
            Ok(())
        }

        fn effect(&self, _workspace: String, redacted: Vec<u8>) -> EffectFuture {
            let this = self.clone();
            Box::pin(async move { this.apply(Change { redacted }).await })
        }
    }

    impl OwnerCommitHandle {
        async fn apply(&self, change: Change) -> Result<(), String> {
            self.health()?;
            let (reply, response) = oneshot::channel();
            self.sender
                .try_send(PendingOwnerCommit { change, reply })
                .map_err(|_| {
                    self.failed.store(true, Ordering::SeqCst);
                    "the record's capacity exhausted; outcome unknown".to_owned()
                })?;
            response.await.map_err(|_| {
                self.failed.store(true, Ordering::SeqCst);
                "the record's reply lost; outcome unknown".to_owned()
            })?
        }
    }
}
