#![allow(
    clippy::unwrap_used,
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

struct Member {
    key: SigningKey,
    keys: ChannelContentKeys,
    /// The app surface the member's writes name, as its page names it.
    surface: Option<[u8; 32]>,
    seq: std::sync::atomic::AtomicU64,
    /// The member's replica of the record.
    document: std::sync::Mutex<automerge::AutoCommit>,
}

impl Member {
    /// Take in the record as the room reads it now.
    async fn catch_up(&self, rooms: &Rooms, room: &str) {
        let bytes = rooms
            .record_document(room, APP, RECORD)
            .await
            .unwrap()
            .unwrap();
        self.document
            .lock()
            .unwrap()
            .merge_incoming(&mut automerge::AutoCommit::load(&bytes).unwrap())
            .unwrap();
    }

    /// The member's next write setting `title`, sealed with the record's
    /// channel keys as its page seals it, and handed to the room the way the
    /// relay hands it every member write.
    async fn writes(&self, rooms: &Rooms, room: &str, value: &str) {
        let mut document = self.document.lock().unwrap().clone();
        let heads = document.get_heads();
        document.put(ROOT, "title", value).unwrap();
        document.commit_edit("fixture", "title");
        let plaintext = collab_types::DocumentMutation::Incremental(document.save_after(&heads))
            .encode()
            .unwrap();
        *self.document.lock().unwrap() = document;
        let artifact = collab_types::document_artifact_id(APP, room, RECORD);
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
            collab_types::document_attestation_id(APP, room, RECORD),
            collab_types::OpKind::Change,
            SystemTime::now(),
            seq,
            metadata,
        )
        .with_surface(self.surface);
        collab_types::sign_op(&mut envelope, &self.key).unwrap();
        let body = postcard::to_stdvec(&NeighbourMsg::AppDocument {
            peer: tilo_peer_state::peer_handle(author.as_bytes()),
            app_id: APP.into(),
            cluster_id: room.into(),
            record_id: RECORD.into(),
            envelope: Box::new(envelope),
        })
        .unwrap();
        rooms
            .absorb(room, self.key.verifying_key().to_bytes(), &body)
            .await
            .unwrap();
    }
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
    member: Member,
}

async fn lounge() -> Lounge {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::cloud_enrollment::host_capabilities());
    let relay = view_read_relay(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let room = rooms.create("Lounge").await.unwrap().room;
    let member_key = SigningKey::from_bytes(&[3; 32]);
    let mut member = cert.clone();
    member.device_public_key = member_key.verifying_key().to_bytes();
    member.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
    admit_view_reader(&rooms, &room, &key, &member).await;
    rooms
        .author_room_record(&room, &cert, (APP, RECORD), |doc| {
            doc.put(ROOT, "title", "Welcome").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let route = tilo_browser_peer::DocumentRoute {
        app_id: APP.into(),
        cluster_id: room.clone(),
        record: RECORD.into(),
    };
    let keys = rooms
        .steward_endpoint_keys(&route, true)
        .await
        .unwrap()
        .unwrap();
    // The surface a member's write names: the one its replica signs with.
    let surface = {
        let state = rooms.room(&room).await.unwrap();
        let mut scratch = state.lock().await.peer().clone();
        let mut document =
            automerge::AutoCommit::load(&scratch.document(APP, RECORD).unwrap()).unwrap();
        let heads = document.get_heads();
        document.put(ROOT, "probe", true).unwrap();
        document.commit_edit("fixture", "title");
        let NeighbourMsg::AppDocument { envelope, .. } = scratch
            .publish_incremental(
                APP,
                RECORD,
                document.save_after(&heads),
                &member_key,
                SystemTime::now(),
            )
            .unwrap()
        else {
            panic!("a document write")
        };
        envelope.surface
    };
    let member = Member {
        key: member_key,
        keys,
        surface,
        seq: std::sync::atomic::AtomicU64::new(0),
        document: std::sync::Mutex::new(automerge::AutoCommit::new()),
    };
    member.catch_up(&rooms, &room).await;
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
    member.writes(&rooms, &room, "graffiti").await;
    assert_eq!(readable(&rooms, &room).await.as_deref(), Some("graffiti"));

    rooms
        .author_room_record(&room, &governor, (APP, RECORD), |doc| {
            doc.put(ROOT, "title", "Welcome").map_err(|e| e.to_string())
        })
        .await
        .expect("the governor writes a sealed record sealed");
    assert_eq!(readable(&rooms, &room).await.as_deref(), Some("Welcome"));
    let state = rooms.room(&room).await.unwrap();
    let published: Vec<Vec<u8>> = state
        .lock()
        .await
        .record
        .outgoing
        .iter()
        .map(|publication| publication.bytes.clone())
        .collect();
    let own = published
        .iter()
        .rev()
        .filter_map(|bytes| postcard::from_bytes::<NeighbourMsg>(bytes).ok())
        .find_map(|message| match message {
            NeighbourMsg::AppDocument {
                record_id,
                envelope,
                ..
            } if record_id == RECORD
                && envelope.author.as_bytes() == &governor.device_public_key =>
            {
                Some(envelope)
            }
            _ => None,
        })
        .expect("the governor published its write");
    let metadata = own.encryption.expect("never in the clear");
    assert!(
        member
            .keys
            .open_content(
                own.artifact,
                &metadata,
                &collab_types::sealed_ciphertext(&own).unwrap(),
            )
            .is_ok(),
        "a member opens the governor's write"
    );
    drop((state, rooms));
    relay.shutdown().await;
}
