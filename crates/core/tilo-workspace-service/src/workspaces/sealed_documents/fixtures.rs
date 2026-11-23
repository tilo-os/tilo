//! A member device writing a record sealed under its channel's keys, for the
//! tests of sealed records and of the bots that read them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture failing is its test failing"
)]

use collab_types::{ContentRetention, SignedOp};
use ed25519_dalek::SigningKey;
use key_group::ContentKeys;
use neighbour_wire::NeighbourMsg;
use tilo_automerge::ApplyIncoming as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use super::super::Workspaces;
use super::super::fixtures::admit_view_reader;

/// Another device of the person whose root (`[1; 32]`) certified the
/// fixture's governor `cert`, admitted to `workspace`.
pub async fn member_device(
    workspaces: &Workspaces,
    workspace: &str,
    (key, cert): (&SigningKey, &DeviceCert),
    seed: u8,
) -> (SigningKey, DeviceCert) {
    let member_key = SigningKey::from_bytes(&[seed; 32]);
    let mut member = cert.clone();
    member.device_public_key = member_key.verifying_key().to_bytes();
    member.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
    admit_view_reader(workspaces, workspace, key, &member).await;
    (member_key, member)
}

/// The write `device`, a member of `workspace`, founds `record` of `app` with,
/// in the clear: a record written before its channel was sealed, which the
/// workspace keeps as any other, so what a member seals onto it later is
/// kept too — and stays shut to a host holding no keys of that channel.
pub async fn plaintext_document(
    workspaces: &Workspaces,
    workspace: &str,
    device: &SigningKey,
    (app, record): (&str, &str),
    change: impl FnOnce(&mut automerge::AutoCommit),
) {
    let surface = workspaces
        .workspace(workspace)
        .await
        .unwrap()
        .lock()
        .await
        .peer()
        .signed_surface(app);
    let mut document = automerge::AutoCommit::new();
    change(&mut document);
    let plaintext = collab_types::DocumentMutation::Snapshot(document.save())
        .encode()
        .unwrap();
    let author = collab_types::Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());
    let mut envelope = SignedOp::new_unsigned(
        plaintext,
        collab_types::document_artifact_id(app, workspace, record),
        author,
        collab_types::document_attestation_id(app, workspace, record),
        collab_types::OpKind::Change,
        tilo_clock::wall(),
        1,
    )
    .with_surface(surface);
    collab_types::sign_op(&mut envelope, device).unwrap();
    let message = NeighbourMsg::AppDocument {
        peer: tilo_peer_state::peer_handle(author.as_bytes()),
        app_id: app.into(),
        cluster_id: workspace.into(),
        document_id: record.into(),
        envelope: Box::new(envelope),
        readers: None,
    };
    let body = postcard::to_stdvec(&message).unwrap();
    workspaces
        .absorb(workspace, device.verifying_key().to_bytes(), &body)
        .await
        .unwrap();
}

/// A member device that holds one record's channel keys and writes the
/// record sealed, as a page does once a key holder admitted it.
pub struct SealingMember {
    key: SigningKey,
    /// The root key of the person whose device this is.
    person: [u8; 32],
    keys: ContentKeys,
    route: (String, String),
    /// The app surface the member's writes name, as its page names it.
    pub(crate) surface: Option<[u8; 32]>,
    seq: std::sync::atomic::AtomicU64,
    /// The member's replica of the record.
    document: std::sync::Mutex<automerge::AutoCommit>,
}

impl SealingMember {
    /// `key`'s device, a member of `workspace`, takes part in the channel of the
    /// record `(app, record)` the governor holds.
    pub async fn join(
        workspaces: &Workspaces,
        workspace: &str,
        (app, record): (&str, &str),
        key: SigningKey,
    ) -> Self {
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: app.into(),
            cluster_id: workspace.into(),
            document: record.into(),
        };
        let keys = workspaces
            .own_route_keys(&route, true)
            .await
            .unwrap()
            .unwrap();
        let peer = workspaces.workspace(workspace).await.unwrap();
        let peer = peer.lock().await;
        // The surface a member's write names: the one its replica signs with.
        let surface = peer.peer().signed_surface(app);
        let person = peer
            .peer()
            .certificates()
            .into_iter()
            .find(|cert| cert.device_public_key == key.verifying_key().to_bytes())
            .expect("the workspace holds the member's certificate")
            .user_public_key;
        drop(peer);
        let member = Self {
            key,
            person,
            keys,
            route: (app.into(), record.into()),
            surface,
            seq: std::sync::atomic::AtomicU64::new(0),
            document: std::sync::Mutex::new(automerge::AutoCommit::new()),
        };
        member.catch_up(workspaces, workspace).await;
        member
    }

    /// The same device taking part in another record of the same app, one
    /// the governor holds no copy of yet: a new part of a chained record,
    /// which a member starts sealed.
    pub async fn part(&self, workspaces: &Workspaces, workspace: &str, record: &str) -> Self {
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: self.route.0.clone(),
            cluster_id: workspace.into(),
            document: record.into(),
        };
        let keys = workspaces
            .own_route_keys(&route, true)
            .await
            .unwrap()
            .unwrap();
        Self {
            key: self.key.clone(),
            person: self.person,
            keys,
            route: (self.route.0.clone(), record.into()),
            surface: self.surface,
            seq: std::sync::atomic::AtomicU64::new(0),
            document: std::sync::Mutex::new(automerge::AutoCommit::new()),
        }
    }

    /// Take in the record as the workspace reads it now.
    pub async fn catch_up(&self, workspaces: &Workspaces, workspace: &str) {
        let bytes = workspaces
            .resource_document(workspace, &self.route.0, &self.route.1)
            .await
            .unwrap()
            .unwrap();
        self.document
            .lock()
            .unwrap()
            .merge_incoming(&mut automerge::AutoCommit::load(&bytes).unwrap())
            .unwrap();
    }

    /// The member's replica replaced by `bytes`, the document as this workspace's copy holds it:
    /// a member's next writes then build on exactly that copy's base, as a page that reopens the
    /// record does.
    pub fn reset_to(&self, bytes: &[u8]) {
        *self.document.lock().unwrap() = automerge::AutoCommit::load(bytes).unwrap();
    }

    /// The member's replica of the record.
    pub fn document(&self) -> automerge::AutoCommit {
        self.document.lock().unwrap().clone()
    }

    /// Open `op`, any write of the record a newcomer is sent (sealed or in
    /// the clear, a snapshot or changes), into `document`.
    pub fn open_into(&self, op: &SignedOp, document: &mut automerge::AutoCommit) {
        self.try_open_into(op, document).unwrap();
    }

    /// [`Self::open_into`], answering why `op` does not go into `document` (such as a change under
    /// a sequence of its writer that `document` holds another change under).
    ///
    /// # Errors
    /// `op` does not open, decode or merge.
    pub fn try_open_into(
        &self,
        op: &SignedOp,
        document: &mut automerge::AutoCommit,
    ) -> Result<(), String> {
        let plaintext = match op.encryption {
            Some(metadata) => self
                .keys
                .open_content(
                    op.artifact,
                    &metadata,
                    &collab_types::sealed_ciphertext(op).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?
                .to_vec(),
            None => op.payload.clone(),
        };
        match collab_types::DocumentMutation::decode(&plaintext)
            .map_err(|error| error.to_string())?
        {
            collab_types::DocumentMutation::Snapshot(bytes) => {
                document
                    .merge_incoming(
                        &mut automerge::AutoCommit::load(&bytes)
                            .map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
            }
            collab_types::DocumentMutation::Incremental(changes) => {
                document
                    .load_incoming(&changes)
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    /// Open `op`, a sealed write of the record, and take in its changes.
    pub fn take_in(&self, op: &SignedOp) {
        let metadata = op.encryption.expect("a sealed write");
        let plaintext = self
            .keys
            .open_content(
                op.artifact,
                &metadata,
                &collab_types::sealed_ciphertext(op).unwrap(),
            )
            .unwrap();
        let collab_types::DocumentMutation::Incremental(changes) =
            collab_types::DocumentMutation::decode(&plaintext).unwrap()
        else {
            panic!("a sealed change")
        };
        self.document
            .lock()
            .unwrap()
            .load_incoming(&changes)
            .unwrap();
    }

    /// The member's next write of the record, `change` made to its replica,
    /// sealed with the record's channel keys as its page seals it, and
    /// handed to the workspace the way the server hands it every member write.
    pub async fn writes<R>(
        &self,
        workspaces: &Workspaces,
        workspace: &str,
        change: impl FnOnce(&mut automerge::AutoCommit) -> R,
    ) -> R {
        let keys = self.route_keys(workspaces, workspace).await;
        let (result, message) = self.seal_with(&keys, workspace, change);
        let body = postcard::to_stdvec(&message).unwrap();
        workspaces
            .absorb(workspace, self.key.verifying_key().to_bytes(), &body)
            .await
            .unwrap();
        result
    }

    /// The keys this member's page seals its next write with, as the page asks its key service
    /// each time: a record that has come to need its own group since the member joined is
    /// sealed under that group, and the workspace says so now.
    async fn route_keys(&self, workspaces: &Workspaces, workspace: &str) -> ContentKeys {
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: self.route.0.clone(),
            cluster_id: workspace.into(),
            document: self.route.1.clone(),
        };
        workspaces
            .own_route_keys(&route, true)
            .await
            .expect("the workspace answers for the record's keys")
            .expect("the member takes part in the record's group")
    }

    /// [`Self::seal`] with `keys`, the member's page's keys for the record now.
    fn seal_with<R>(
        &self,
        keys: &ContentKeys,
        workspace: &str,
        change: impl FnOnce(&mut automerge::AutoCommit) -> R,
    ) -> (R, NeighbourMsg) {
        let mut document = self.document();
        let heads = document.get_heads();
        let result = change(&mut document);
        let changes = document.save_after(&heads);
        *self.document.lock().unwrap() = document;
        (result, self.seal_changes_with(keys, workspace, changes))
    }

    /// The member folds the record's sealed history, as the workspace holds it
    /// now, into one signed checkpoint under its own author: what its engine
    /// publishes in place of the writes the checkpoint covers.
    pub async fn compact(&self, workspaces: &Workspaces, workspace: &str) -> Vec<NeighbourMsg> {
        let (app, record) = (self.route.0.as_str(), self.route.1.as_str());
        let artifact = collab_types::document_artifact_id(app, workspace, record);
        let mut candidate = workspaces
            .workspace(workspace)
            .await
            .unwrap()
            .lock()
            .await
            .peer()
            .clone();
        let now = tilo_clock::wall();
        let mut ops = candidate
            .publication()
            .draft_sealed_compaction(
                (app, record),
                None,
                None,
                &mut |op| self.open(op),
                &mut |frame| {
                    self.keys
                        .seal_content(artifact, ContentRetention::of_frame(frame), frame)
                        .map_err(|error| error.to_string())
                },
                self.key.verifying_key().to_bytes(),
                now,
            )
            .unwrap();
        for op in &mut ops {
            collab_types::sign_op(op, &self.key).unwrap();
        }
        let messages = candidate
            .publication_mut()
            .admit_sealed_compaction(app, record, None, ops, now)
            .unwrap();
        // The compaction took this device's next place in the record's
        // sequence, as its engine's own writes see it: the writes after it
        // carry the numbers after that.
        let Some(NeighbourMsg::AppDocument { envelope, .. }) = messages.first() else {
            panic!("a compaction is a document write")
        };
        self.seq
            .store(envelope.seq, std::sync::atomic::Ordering::Relaxed);
        messages
    }

    /// The member's compaction of `history`, the document's sealed history as the member holds
    /// it, numbered as the last of its own writes there: what its engine sends in place of
    /// exactly those writes, whatever the governor's copy holds.
    pub async fn compacts(
        &self,
        workspaces: &Workspaces,
        workspace: &str,
        history: Vec<SignedOp>,
    ) -> NeighbourMsg {
        let (app, document) = (self.route.0.as_str(), self.route.1.as_str());
        let artifact = collab_types::document_artifact_id(app, workspace, document);
        let author = self.key.verifying_key().to_bytes();
        let seq = history
            .iter()
            .filter(|op| op.author.as_bytes() == &author)
            .map(|op| op.seq)
            .max()
            .unwrap_or(0);
        let candidate = workspaces
            .workspace(workspace)
            .await
            .unwrap()
            .lock()
            .await
            .peer()
            .clone();
        let mut ops = candidate
            .publication()
            .draft_sealed_compaction_of(
                (app, document),
                None,
                history,
                &mut |op| self.open(op),
                &mut |frame| {
                    self.keys
                        .seal_content(artifact, ContentRetention::of_frame(frame), frame)
                        .map_err(|error| error.to_string())
                },
                (author, seq),
                tilo_clock::wall(),
            )
            .unwrap();
        assert_eq!(ops.len(), 1, "a small document compacts into one write");
        let mut op = ops.remove(0);
        collab_types::sign_op(&mut op, &self.key).unwrap();
        NeighbourMsg::AppDocument {
            peer: tilo_peer_state::peer_handle(&author),
            app_id: app.into(),
            cluster_id: workspace.into(),
            document_id: document.into(),
            envelope: Box::new(op),
            readers: None,
        }
    }

    /// Open `op`, any write of the record's history, under any epoch this
    /// member holds, a compaction included.
    pub(super) fn open(&self, op: &SignedOp) -> Result<Vec<u8>, String> {
        let Some(metadata) = op.encryption else {
            return Ok(op.payload.clone());
        };
        self.keys
            .open_content(
                op.artifact,
                &metadata,
                &collab_types::sealed_ciphertext(op).map_err(|error| error.to_string())?,
            )
            .map(|plaintext| plaintext.to_vec())
            .map_err(|error| error.to_string())
    }

    /// The same page reloaded: the device and its place in the record's sequence kept, its
    /// replica read afresh from the workspace, as a page that reloads opens the record again, and
    /// its changes written under the same Automerge writer, as the shared desktop writes one per
    /// device.
    pub async fn reloads(&self, workspaces: &Workspaces, workspace: &str) -> Self {
        let actor = self.document.lock().unwrap().get_actor().clone();
        let page = Self {
            key: self.key.clone(),
            person: self.person,
            keys: self.keys.clone(),
            route: self.route.clone(),
            surface: self.surface,
            seq: std::sync::atomic::AtomicU64::new(
                self.seq.load(std::sync::atomic::Ordering::Relaxed),
            ),
            document: std::sync::Mutex::new(automerge::AutoCommit::new().with_actor(actor)),
        };
        page.catch_up(workspaces, workspace).await;
        page
    }

    /// The person whose certified device this member is.
    #[must_use]
    pub fn person(&self) -> [u8; 32] {
        self.person
    }

    /// The member's next write of the record, `change` made to its replica,
    /// sealed and signed as its page publishes it.
    pub fn seal<R>(
        &self,
        workspace: &str,
        change: impl FnOnce(&mut automerge::AutoCommit) -> R,
    ) -> (R, NeighbourMsg) {
        let mut document = self.document();
        let heads = document.get_heads();
        let result = change(&mut document);
        let changes = document.save_after(&heads);
        *self.document.lock().unwrap() = document;
        (result, self.seal_changes(workspace, changes))
    }

    /// The member's first write of a part it starts, as a device publishes it: the part whole,
    /// sealed as the checkpoint that founds the record at a device holding none of it.
    pub fn founds<R>(
        &self,
        workspace: &str,
        change: impl FnOnce(&mut automerge::AutoCommit) -> R,
    ) -> (R, NeighbourMsg) {
        let mut document = self.document();
        let result = change(&mut document);
        let whole = collab_types::DocumentMutation::Snapshot(document.save());
        *self.document.lock().unwrap() = document;
        (
            result,
            self.seal_mutation(&self.keys, workspace, &whole, ContentRetention::Checkpoint),
        )
    }

    /// The member's next write of the record carrying `changes`, whichever
    /// they are, sealed and signed as its page publishes it, and handed to the
    /// workspace the way the server hands it every member write.
    pub async fn sends_changes(&self, workspaces: &Workspaces, workspace: &str, changes: Vec<u8>) {
        let keys = self.route_keys(workspaces, workspace).await;
        let message = self.seal_changes_with(&keys, workspace, changes);
        let body = postcard::to_stdvec(&message).unwrap();
        workspaces
            .absorb(workspace, self.key.verifying_key().to_bytes(), &body)
            .await
            .unwrap();
    }

    /// `changes` as the member's next write of the record, sealed and signed.
    fn seal_changes(&self, workspace: &str, changes: Vec<u8>) -> NeighbourMsg {
        self.seal_changes_with(&self.keys, workspace, changes)
    }

    /// [`Self::seal_changes`] sealed with `keys`.
    fn seal_changes_with(
        &self,
        keys: &ContentKeys,
        workspace: &str,
        changes: Vec<u8>,
    ) -> NeighbourMsg {
        self.seal_mutation(
            keys,
            workspace,
            &collab_types::DocumentMutation::Incremental(changes),
            ContentRetention::Delta,
        )
    }

    /// `mutation` as the member's next write of the record, sealed under `keys` as `retention`
    /// and signed.
    fn seal_mutation(
        &self,
        keys: &ContentKeys,
        workspace: &str,
        mutation: &collab_types::DocumentMutation,
        retention: ContentRetention,
    ) -> NeighbourMsg {
        let (app, record) = (self.route.0.as_str(), self.route.1.as_str());
        let plaintext = mutation.encode().unwrap();
        let artifact = collab_types::document_artifact_id(app, workspace, record);
        let (metadata, payload) = keys.seal_content(artifact, retention, &plaintext).unwrap();
        let author =
            collab_types::Ed25519PublicKey::from_bytes(self.key.verifying_key().to_bytes());
        let seq = self
            .seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .saturating_add(1);
        let mut envelope = SignedOp::new_unsigned_with_encryption(
            payload,
            artifact,
            author,
            collab_types::document_attestation_id(app, workspace, record),
            collab_types::OpKind::Change,
            tilo_clock::wall(),
            seq,
            metadata,
        )
        .with_surface(self.surface);
        collab_types::sign_op(&mut envelope, &self.key).unwrap();
        NeighbourMsg::AppDocument {
            peer: tilo_peer_state::peer_handle(author.as_bytes()),
            app_id: app.into(),
            cluster_id: workspace.into(),
            document_id: record.into(),
            envelope: Box::new(envelope),
            readers: None,
        }
    }
}

/// The newest write of `record` the governor published to `workspace`'s members, whoever it
/// signs for: a host's checkpoint is its own, and a bot's change of the record is the bot's.
pub async fn governor_published(
    workspaces: &Workspaces,
    workspace: &str,
    record: &str,
    _governor: &DeviceCert,
) -> Option<SignedOp> {
    let state = workspaces.workspace(workspace).await.unwrap();
    let published: Vec<Vec<u8>> = state
        .lock()
        .await
        .record
        .outgoing
        .iter()
        .map(|publication| publication.bytes.to_vec())
        .collect();
    published
        .iter()
        .rev()
        .filter_map(|bytes| {
            tilo_failure::ignored!(
                "test support: other publications are skipped",
                NeighbourMsg::decode(bytes).ok()
            )
        })
        .find_map(|message| match message {
            NeighbourMsg::AppDocument {
                document_id,
                envelope,
                ..
            } if document_id == record => Some(*envelope),
            _ => None,
        })
}
