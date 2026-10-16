#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the member's helpers are part of the test's assertions"
)]

use super::super::Rooms;
use super::super::tests::{admit_view_reader, governor_with, view_read_relay};
use super::*;
use automerge::{ROOT, ReadDoc as _, transaction::Transactable as _};
use collab_types::CommitNamed as _;
use tilo_automerge::ApplyIncoming as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

const APP: &str = "so.tilo.notes";
const RECORD: &str = "welcome";

fn title(bytes: &[u8]) -> Option<String> {
    automerge::AutoCommit::load(bytes)
        .ok()?
        .get(ROOT, "title")
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
}

/// Another device of the person whose root (`[1; 32]`) certified the
/// fixture's governor `cert`, admitted to `room`.
pub(crate) async fn member_device(
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
/// record sealed, as a page does once the room's steward admitted it.
pub(crate) struct SealingMember {
    key: SigningKey,
    keys: ChannelContentKeys,
    route: (String, String),
    /// The app surface the member's writes name, as its page names it.
    surface: Option<[u8; 32]>,
    seq: std::sync::atomic::AtomicU64,
    /// The member's replica of the record.
    document: std::sync::Mutex<automerge::AutoCommit>,
}

impl SealingMember {
    /// `key`'s device, a member of `room`, takes part in the channel of the
    /// record `(app, record)` the governor holds in the clear so far.
    pub(crate) async fn join(
        rooms: &Rooms,
        room: &str,
        (app, record): (&str, &str),
        key: SigningKey,
    ) -> Self {
        let route = tilo_browser_peer::DocumentRoute {
            app_id: app.into(),
            cluster_id: room.into(),
            record: record.into(),
        };
        let keys = rooms
            .steward_endpoint_keys(&route, true)
            .await
            .unwrap()
            .unwrap();
        // The surface a member's write names: the one its replica signs with.
        let surface = {
            let state = rooms.room(room).await.unwrap();
            let mut scratch = state.lock().await.peer().clone();
            let mut document =
                automerge::AutoCommit::load(&scratch.document(app, record).unwrap()).unwrap();
            let heads = document.get_heads();
            document.put(ROOT, "probe", true).unwrap();
            document.commit_edit("fixture", "probe");
            let NeighbourMsg::AppDocument { envelope, .. } = scratch
                .publish_incremental(
                    app,
                    record,
                    document.save_after(&heads),
                    &key,
                    SystemTime::now(),
                )
                .unwrap()
            else {
                panic!("a document write")
            };
            envelope.surface
        };
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
    pub(crate) async fn part(&self, rooms: &Rooms, room: &str, record: &str) -> Self {
        let route = tilo_browser_peer::DocumentRoute {
            app_id: self.route.0.clone(),
            cluster_id: room.into(),
            record: record.into(),
        };
        let keys = rooms
            .steward_endpoint_keys(&route, true)
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
    pub(crate) async fn catch_up(&self, rooms: &Rooms, room: &str) {
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
    pub(crate) fn document(&self) -> automerge::AutoCommit {
        self.document.lock().unwrap().clone()
    }

    /// Open `op`, any write of the record a newcomer is sent (sealed or in
    /// the clear, a snapshot or changes), into `document`.
    pub(crate) fn open_into(&self, op: &SignedOp, document: &mut automerge::AutoCommit) {
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
    pub(crate) fn take_in(&self, op: &SignedOp) {
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
    pub(crate) async fn writes<R>(
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

    /// The member's next write of the record, `change` made to its replica,
    /// sealed and signed as its page publishes it.
    pub(crate) fn seal<R>(
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
pub(crate) async fn governor_published(
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

async fn set_title(member: &SealingMember, rooms: &Rooms, room: &str, value: &str) {
    member
        .writes(rooms, room, |document| {
            document.put(ROOT, "title", value).unwrap();
            document.commit_edit("fixture", "title");
        })
        .await;
}

async fn readable(rooms: &Rooms, room: &str) -> Option<String> {
    title(&rooms.record_document(room, APP, RECORD).await.unwrap()?)
}

/// A node governing a room, the room's record of [`APP`] it holds, and a
/// member device that writes it sealed under the keys the node stewards.
struct Lounge {
    _dir: tempfile::TempDir,
    relay: crate::daemon::Relay,
    rooms: Rooms,
    room: String,
    governor: DeviceCert,
    member: SealingMember,
}

async fn lounge() -> Lounge {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::cloud_enrollment::host_capabilities());
    let relay = view_read_relay(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let room = rooms.create("Lounge").await.unwrap().room;
    let (member_key, _) = member_device(&rooms, &room, (&key, &cert), 3).await;
    rooms
        .author_room_record(&room, &cert, (APP, RECORD), |doc| {
            doc.put(ROOT, "title", "Welcome").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let member = SealingMember::join(&rooms, &room, (APP, RECORD), member_key).await;
    Lounge {
        _dir: dir,
        relay,
        rooms,
        room,
        governor: cert,
        member,
    }
}

/// A node governs a room whose record its members write sealed. It reads
/// the record through the channel keys it holds as steward and resets it
/// with a write of its own, sealed like theirs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_node_reads_and_resets_a_record_its_members_seal() {
    let Lounge {
        _dir,
        relay,
        rooms,
        room,
        governor,
        member,
    } = lounge().await;
    set_title(&member, &rooms, &room, "graffiti").await;
    assert_eq!(readable(&rooms, &room).await.as_deref(), Some("graffiti"));

    rooms
        .author_room_record(&room, &governor, (APP, RECORD), |doc| {
            doc.put(ROOT, "title", "Welcome").map_err(|e| e.to_string())
        })
        .await
        .expect("the governor writes a sealed record sealed");
    assert_eq!(readable(&rooms, &room).await.as_deref(), Some("Welcome"));
    let own = governor_published(&rooms, &room, RECORD, &governor)
        .await
        .expect("the governor published its write");
    assert!(own.encryption.is_some(), "never in the clear");
    member.take_in(&own);
    assert_eq!(
        title(&member.document().save()).as_deref(),
        Some("Welcome"),
        "a member opens the governor's write"
    );
    drop(rooms);
    relay.shutdown().await;
}

/// A public room's chat takes thousands of writes within hours. Past three
/// quarters of a sealed history's bound the node compacts it into one sealed
/// checkpoint, so its copy goes on following the record, and reads it whole.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_node_compacts_a_sealed_history_before_it_fills() {
    let Lounge {
        _dir,
        relay,
        rooms,
        room,
        member,
        ..
    } = lounge().await;
    // More writes than the history's bound (4096) holds.
    for index in 0..4200 {
        set_title(&member, &rooms, &room, &format!("line {index}")).await;
    }
    assert_eq!(readable(&rooms, &room).await.as_deref(), Some("line 4199"));
    let state = rooms.room(&room).await.unwrap();
    let due = state.lock().await.peer().sealed_records_to_compact();
    assert!(due.is_empty(), "the history was compacted: {due:?}");
    // Members hold every write it covers: the compaction stays with the
    // governor's copy and is never pushed to them.
    let published = state
        .lock()
        .await
        .record
        .outgoing
        .iter()
        .filter_map(|publication| postcard::from_bytes::<NeighbourMsg>(&publication.bytes).ok())
        .filter(|message| {
            matches!(message, NeighbourMsg::AppDocument { envelope, .. }
                if envelope.kind == collab_types::OpKind::Compaction)
        })
        .count();
    assert_eq!(published, 0, "a compaction is not published to members");
    drop((state, rooms));
    relay.shutdown().await;
}

/// Every write of `record` the relay retains for `workspace_id`, in replay
/// order: what a newcomer is sent.
async fn retained_writes(
    relay: &crate::daemon::Relay,
    workspace_id: u64,
    record: &str,
) -> Vec<SignedOp> {
    relay
        .room_records()
        .retained(workspace_id)
        .await
        .unwrap()
        .into_iter()
        .filter_map(
            |retained| match postcard::from_bytes::<NeighbourMsg>(&retained.body) {
                Ok(NeighbourMsg::AppDocument {
                    record_id,
                    envelope,
                    ..
                }) if record_id == record => Some(*envelope),
                _ => None,
            },
        )
        .collect()
}

/// A record its members wrote sealed for a long time is folded by the node
/// that stewards its channel: a newcomer is sent one sealed snapshot and the
/// writes since, not every write ever made, and they open to the record as
/// its members wrote it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Relay::shutdown consumes the relay after the assertions"
)]
async fn a_node_folds_the_sealed_history_a_newcomer_replays() {
    const WRITES: usize = 400;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::cloud_enrollment::host_capabilities());
    let relay = view_read_relay(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let created = rooms.create("Lounge").await.unwrap();
    let (room, workspace_id) = (created.room, created.workspace_id);
    let (member_key, member_cert) = member_device(&rooms, &room, (&key, &cert), 3).await;
    rooms
        .author_room_record(&room, &cert, (APP, RECORD), |doc| {
            doc.put(ROOT, "title", "Welcome").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let member = SealingMember::join(&rooms, &room, (APP, RECORD), member_key.clone()).await;
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&member_key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let client = crate::RelayClient::connect_to_pinned_host(
        endpoint,
        relay.node_addr(),
        member_cert,
        member_key.clone(),
        tilo_user_identity::RevocationList::new(),
    )
    .await
    .unwrap();
    client
        .send(crate::Envelope::Subscribe { workspace_id })
        .unwrap();
    tilo_test_env::within("the member's subscription", async {
        while !matches!(client.recv().await, Some(crate::Envelope::Ready { .. })) {}
    })
    .await;
    for index in 0..WRITES {
        let ((), message) = member.seal(&room, |doc| {
            doc.put(ROOT, "title", format!("line {index}")).unwrap();
            doc.commit_edit("fixture", "title");
        });
        let (channel, _) = neighbour_wire::retention(&message, false).unwrap();
        client
            .send(crate::Envelope::Delta {
                workspace_id,
                author_device_public_key: member_key.verifying_key().to_bytes(),
                authored_at_ns: tilo_clock::unix_now_ns(),
                delta: crate::CoeditPayload::RetainedDelta {
                    channel,
                    body: postcard::to_stdvec(&message).unwrap(),
                }
                .encode()
                .unwrap(),
                signature: [0; 64],
            })
            .unwrap();
    }
    // What a newcomer is sent, opened: every write, and the newest last.
    let newest = format!("line {}", WRITES - 1);
    let newcomer_reads = |held: &[SignedOp]| {
        let mut newcomer = automerge::AutoCommit::new();
        for op in held {
            member.open_into(op, &mut newcomer);
        }
        title(&newcomer.save())
    };
    let held = tilo_test_env::within("the node folds the record's sealed history", async {
        loop {
            let held = retained_writes(&relay, workspace_id, RECORD).await;
            let folded = held.iter().any(|op| {
                op.author.as_bytes() == &cert.device_public_key && op.encryption.is_some()
            });
            if folded
                && held.len() < WRITES / 4
                && newcomer_reads(&held).as_deref() == Some(newest.as_str())
            {
                break held;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    })
    .await;
    assert!(
        held.iter().skip(1).all(|op| op.encryption.is_some()),
        "past the record's cleartext start, the members' writes and their snapshot stay sealed"
    );
    drop(rooms);
    relay.shutdown().await;
}

/// A chat kept as a chain of parts starts each part with a member's sealed
/// write, under keys the node stewards. The node takes in every part as it
/// starts, however many there are, and reads each.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_node_follows_every_part_its_members_start_sealed() {
    let Lounge {
        _dir,
        relay,
        rooms,
        room,
        member,
        ..
    } = lounge().await;
    for index in 0..40 {
        let record = neighbour_wire::record_part(RECORD, index);
        let part = member.part(&rooms, &room, &record).await;
        for line in 0..3 {
            set_title(&part, &rooms, &room, &format!("part {index} line {line}")).await;
        }
        let held = rooms.record_document(&room, APP, &record).await.unwrap();
        assert_eq!(
            held.as_deref().and_then(title).as_deref(),
            Some(format!("part {index} line 2").as_str()),
            "the node holds part {index}"
        );
    }
    drop(rooms);
    relay.shutdown().await;
}
