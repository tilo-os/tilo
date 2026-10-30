//! A member device writing a record sealed under its channel's keys, for the
//! tests of sealed records and of the bots that read them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture failing is its test failing"
)]

use std::time::SystemTime;

use channel_crypto::ChannelContentKeys;
use collab_types::{ContentRetention, SignedOp};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_automerge::ApplyIncoming as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use super::super::Rooms;
use super::super::fixtures::admit_view_reader;

/// Another device of the person whose root (`[1; 32]`) certified the
/// fixture's governor `cert`, admitted to `room`.
pub async fn member_device(
    rooms: &Rooms,
    room: &str,
    (key, cert): (&SigningKey, &DeviceCert),
    seed: u8,
) -> (SigningKey, DeviceCert) {
    let member_key = SigningKey::from_bytes(&[seed; 32]);
    let mut member = cert.clone();
    member.device_public_key = member_key.verifying_key().to_bytes();
    member.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
    admit_view_reader(rooms, room, key, &member).await;
    (member_key, member)
}

/// A member device that holds one record's channel keys and writes the
/// record sealed, as a page does once a participant admitted it.
pub struct SealingMember {
    key: SigningKey,
    keys: ChannelContentKeys,
    route: (String, String),
    /// The app surface the member's writes name, as its page names it.
    pub(crate) surface: Option<[u8; 32]>,
    seq: std::sync::atomic::AtomicU64,
    /// The member's replica of the record.
    document: std::sync::Mutex<automerge::AutoCommit>,
}

impl SealingMember {
    /// `key`'s device, a member of `room`, takes part in the channel of the
    /// record `(app, record)` the governor holds.
    pub async fn join(
        rooms: &Rooms,
        room: &str,
        (app, record): (&str, &str),
        key: SigningKey,
    ) -> Self {
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: app.into(),
            cluster_id: room.into(),
            record: record.into(),
        };
        let keys = rooms
            .own_endpoint_keys(&route, true)
            .await
            .unwrap()
            .unwrap();
        // The surface a member's write names: the one its replica signs with.
        let surface = rooms
            .room(room)
            .await
            .unwrap()
            .lock()
            .await
            .peer()
            .signed_surface(app);
        let member = Self {
            key,
            keys,
            route: (app.into(), record.into()),
            surface,
            seq: std::sync::atomic::AtomicU64::new(0),
            document: std::sync::Mutex::new(automerge::AutoCommit::new()),
        };
        member.catch_up(rooms, room).await;
        member
    }

    /// The same device taking part in another record of the same app, one
    /// the governor holds no copy of yet: a new part of a chained record,
    /// which a member starts sealed.
    pub async fn part(&self, rooms: &Rooms, room: &str, record: &str) -> Self {
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: self.route.0.clone(),
            cluster_id: room.into(),
            record: record.into(),
        };
        let keys = rooms
            .own_endpoint_keys(&route, true)
            .await
            .unwrap()
            .unwrap();
        Self {
            key: self.key.clone(),
            keys,
            route: (self.route.0.clone(), record.into()),
            surface: self.surface,
            seq: std::sync::atomic::AtomicU64::new(0),
            document: std::sync::Mutex::new(automerge::AutoCommit::new()),
        }
    }

    /// Take in the record as the room reads it now.
    pub async fn catch_up(&self, rooms: &Rooms, room: &str) {
        let bytes = rooms
            .record_document(room, &self.route.0, &self.route.1)
            .await
            .unwrap()
            .unwrap();
        self.document
            .lock()
            .unwrap()
            .merge_incoming(&mut automerge::AutoCommit::load(&bytes).unwrap())
            .unwrap();
    }

    /// The member's replica of the record.
    pub fn document(&self) -> automerge::AutoCommit {
        self.document.lock().unwrap().clone()
    }

    /// Open `op`, any write of the record a newcomer is sent (sealed or in
    /// the clear, a snapshot or changes), into `document`.
    pub fn open_into(&self, op: &SignedOp, document: &mut automerge::AutoCommit) {
        let plaintext = match op.encryption {
            Some(metadata) => self
                .keys
                .open_content(
                    op.artifact,
                    &metadata,
                    &collab_types::sealed_ciphertext(op).unwrap(),
                )
                .unwrap()
                .to_vec(),
            None => op.payload.clone(),
        };
        match collab_types::DocumentMutation::decode(&plaintext).unwrap() {
            collab_types::DocumentMutation::Snapshot(bytes) => {
                document
                    .merge_incoming(&mut automerge::AutoCommit::load(&bytes).unwrap())
                    .unwrap();
            }
            collab_types::DocumentMutation::Incremental(changes) => {
                document.load_incoming(&changes).unwrap();
            }
        }
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
    /// handed to the room the way the relay hands it every member write.
    pub async fn writes<R>(
        &self,
        rooms: &Rooms,
        room: &str,
        change: impl FnOnce(&mut automerge::AutoCommit) -> R,
    ) -> R {
        let (result, message) = self.seal(room, change);
        let body = postcard::to_stdvec(&message).unwrap();
        rooms
            .absorb(room, self.key.verifying_key().to_bytes(), &body)
            .await
            .unwrap();
        result
    }

    /// The member folds the record's sealed history, as the room holds it
    /// now, into one signed checkpoint under its own author: what its engine
    /// publishes in place of the writes the checkpoint covers.
    pub async fn compact(&self, rooms: &Rooms, room: &str) -> NeighbourMsg {
        let (app, record) = (self.route.0.as_str(), self.route.1.as_str());
        let artifact = collab_types::document_artifact_id(app, room, record);
        let mut candidate = rooms.room(room).await.unwrap().lock().await.peer().clone();
        let message = candidate
            .publication_mut()
            .compact_sealed_history(
                app,
                record,
                &mut |op| self.open(op),
                &mut |snapshot| {
                    self.keys
                        .seal_content(artifact, ContentRetention::Checkpoint, snapshot)
                        .map_err(|error| error.to_string())
                },
                &self.key,
                SystemTime::now(),
            )
            .unwrap();
        // The compaction took this device's next place in the record's
        // sequence, as its engine's own writes see it: the writes after it
        // carry the numbers after that.
        let NeighbourMsg::AppDocument { envelope, .. } = &message else {
            panic!("a compaction is a document write")
        };
        self.seq
            .store(envelope.seq, std::sync::atomic::Ordering::Relaxed);
        message
    }

    /// Open `op`, any write of the record's history, under any epoch this
    /// member holds, a compaction included.
    fn open(&self, op: &SignedOp) -> Result<Vec<u8>, String> {
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

    /// The member's next write of the record, `change` made to its replica,
    /// sealed and signed as its page publishes it.
    pub fn seal<R>(
        &self,
        room: &str,
        change: impl FnOnce(&mut automerge::AutoCommit) -> R,
    ) -> (R, NeighbourMsg) {
        let (app, record) = (self.route.0.as_str(), self.route.1.as_str());
        let mut document = self.document();
        let heads = document.get_heads();
        let result = change(&mut document);
        let plaintext = collab_types::DocumentMutation::Incremental(document.save_after(&heads))
            .encode()
            .unwrap();
        *self.document.lock().unwrap() = document;
        let artifact = collab_types::document_artifact_id(app, room, record);
        let (metadata, payload) = self
            .keys
            .seal_content(artifact, ContentRetention::Delta, &plaintext)
            .unwrap();
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
            collab_types::document_attestation_id(app, room, record),
            collab_types::OpKind::Change,
            SystemTime::now(),
            seq,
            metadata,
        )
        .with_surface(self.surface);
        collab_types::sign_op(&mut envelope, &self.key).unwrap();
        let message = NeighbourMsg::AppDocument {
            peer: tilo_peer_state::peer_handle(author.as_bytes()),
            app_id: app.into(),
            cluster_id: room.into(),
            record_id: record.into(),
            envelope: Box::new(envelope),
        };
        (result, message)
    }
}

/// The newest write of `record` the governor `governor` published to
/// `room`'s members.
pub async fn governor_published(
    rooms: &Rooms,
    room: &str,
    record: &str,
    governor: &DeviceCert,
) -> Option<SignedOp> {
    let state = rooms.room(room).await.unwrap();
    let published: Vec<Vec<u8>> = state
        .lock()
        .await
        .record
        .outgoing
        .iter()
        .map(|publication| publication.bytes.clone())
        .collect();
    published
        .iter()
        .rev()
        // optional-decode: test support (compiled only for tests); other publications are skipped
        .filter_map(|bytes| postcard::from_bytes::<NeighbourMsg>(bytes).ok())
        .find_map(|message| match message {
            NeighbourMsg::AppDocument {
                record_id,
                envelope,
                ..
            } if record_id == record
                && envelope.author.as_bytes() == &governor.device_public_key =>
            {
                Some(*envelope)
            }
            _ => None,
        })
}
