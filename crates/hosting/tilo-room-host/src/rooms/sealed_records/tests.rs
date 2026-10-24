#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the member's helpers are part of the test's assertions"
)]

use super::super::Rooms;
use super::super::tests::{governor_with, test_host};
use super::fixtures::{SealingMember, governor_published, member_device};
use super::*;
use automerge::{ROOT, ReadDoc as _, transaction::Transactable as _};
use collab_types::CommitNamed as _;
use tilo_automerge::ApplyIncoming as _;
use tilo_user_identity::DeviceCert;

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
    relay: crate::rooms::fixtures::TestHost,
    rooms: Rooms,
    room: String,
    governor: DeviceCert,
    member: SealingMember,
}

async fn lounge() -> Lounge {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::cloud_enrollment::host_capabilities());
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().clone();
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
    let due = state
        .lock()
        .await
        .peer()
        .publication()
        .sealed_records_to_compact();
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
    relay: &crate::rooms::fixtures::TestHost,
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
async fn a_node_folds_the_sealed_history_a_newcomer_replays() {
    const WRITES: usize = 400;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::cloud_enrollment::host_capabilities());
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().clone();
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
    assert!(held.iter().all(holds_nothing_in_the_clear));
    drop(rooms);
    relay.shutdown().await;
}

/// Whether `op` is sealed, or holds nothing: the checkpoint a record is
/// born from.
fn holds_nothing_in_the_clear(op: &SignedOp) -> bool {
    op.encryption.is_some()
        || matches!(
            collab_types::DocumentMutation::decode(&op.payload),
            Ok(collab_types::DocumentMutation::Snapshot(bytes))
                if automerge::AutoCommit::load(&bytes).unwrap().get_heads().is_empty()
        )
}

/// Copy what `from` holds into `to`, keeping files `to` has already.
fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            copy_dir(&entry.path(), &target);
        } else if !target.exists() {
            // A file removed since it was listed is not part of this state.
            let _ = std::fs::copy(entry.path(), &target);
        }
    }
}

/// The node is stopped hard a moment after it took in members' sealed
/// writes, with no governance change since. The room as it opens from disk,
/// before anything is replayed, already holds them: replaying the relay's
/// copy at a start may not bring a write back (the app surface it is
/// decided against is not there yet), and a later write building on it
/// would leave the record with a gap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn members_writes_the_node_took_in_survive_a_hard_stop() {
    let Lounge {
        _dir,
        relay,
        rooms,
        room,
        governor,
        member,
    } = lounge().await;
    for line in ["graffiti", "more graffiti"] {
        set_title(&member, &rooms, &room, line).await;
    }
    assert_eq!(
        readable(&rooms, &room).await.as_deref(),
        Some("more graffiti")
    );
    let path = rooms.room(&room).await.unwrap().lock().await.path.clone();
    let key = governor_with(tilo_user_identity::cloud_enrollment::host_capabilities()).0;

    // What a hard stop leaves on disk, copied (the room file before the
    // files it names) and opened as the next start opens it.
    let deadline = std::time::Instant::now() + super::super::KEEP_FOLLOWED * 30;
    loop {
        let stopped = tempfile::tempdir().unwrap();
        let copy = stopped.path().join(path.file_name().unwrap());
        std::fs::copy(&path, &copy).unwrap();
        copy_dir(path.parent().unwrap(), stopped.path());
        let held = match GovernedRoom::open(copy, &governor, &key) {
            Ok(reopened) => reopened.readable_document(APP, RECORD).await.unwrap(),
            // Copied while the room wrote its next state.
            Err(_) => None,
        };
        if held.as_deref().and_then(title).as_deref() == Some("more graffiti") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the members' writes never reached the room's own file"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    drop(rooms);
    relay.shutdown().await;
}

/// App surfaces as a relay has them while it fetches one from the registry:
/// none until the fetch lands, then one that admits a sealed write.
struct LateSurface(std::sync::Arc<std::sync::atomic::AtomicBool>);

struct AnySurface;

impl collab_types::ReceiptSurface for AnySurface {
    fn kind(&self) -> &'static str {
        ""
    }
    fn verb(&self, _: &str) -> Option<(tilo_action_policy::ActionKind, Option<String>)> {
        None
    }
    fn editable(&self, _: &[tilo_actions::PathStep]) -> bool {
        false
    }
    fn footprint(&self, _: &collab_types::ClaimedAction) -> Vec<collab_types::UnitPath> {
        Vec::new()
    }
}

impl collab_types::SurfaceSource for LateSurface {
    fn surface(
        &self,
        _: &collab_types::Record<'_>,
        _: Option<&str>,
        _: Option<&[u8; 32]>,
    ) -> Option<std::sync::Arc<dyn collab_types::ReceiptSurface>> {
        self.0.load(std::sync::atomic::Ordering::SeqCst).then(|| {
            std::sync::Arc::new(AnySurface) as std::sync::Arc<dyn collab_types::ReceiptSurface>
        })
    }
}

/// Members write while the app surface their writes name is still being
/// fetched, as after a restart: the governor holds the writes, more than its
/// held queue keeps, and lets the oldest go. Once the surface arrives the
/// governor replays what the relay retains and takes in every write, those
/// it let go included, without waiting for another write to land.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_held_for_a_late_surface_are_all_taken_in_once_it_arrives() {
    const WRITES: usize = tilo_peer_state::MAX_HELD_INPUTS + 16;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::cloud_enrollment::host_capabilities());
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().clone();
    let created = rooms.create("Lounge").await.unwrap();
    let (room, workspace_id) = (created.room, created.workspace_id);
    let (member_key, member_cert) = member_device(&rooms, &room, (&key, &cert), 3).await;
    rooms
        .author_room_record(&room, &cert, (APP, RECORD), |doc| {
            doc.put(ROOT, "title", "Welcome").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let mut member = SealingMember::join(&rooms, &room, (APP, RECORD), member_key.clone()).await;
    // The member's page names a surface; the governor has not got it yet.
    member.surface = Some([7; 32]);
    let here = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    rooms
        .room(&room)
        .await
        .unwrap()
        .lock()
        .await
        .peer
        .set_surfaces(collab_types::Surfaces::from_source(std::sync::Arc::new(
            LateSurface(std::sync::Arc::clone(&here)),
        )));
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
    let mut followed = rooms.record_writes();
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
    tilo_test_env::within("the governor follows every write", async {
        let mut seen = 0;
        while seen < WRITES {
            if followed.recv().await.unwrap().record == RECORD {
                seen += 1;
            }
        }
    })
    .await;
    assert_eq!(
        readable(&rooms, &room).await.as_deref(),
        Some("Welcome"),
        "every write waits for the surface"
    );

    here.store(true, std::sync::atomic::Ordering::SeqCst);
    crate::surfaces::arrived();
    let newest = format!("line {}", WRITES - 1);
    tilo_test_env::within(
        "the governor takes in every write once the surface is here",
        async {
            while readable(&rooms, &room).await.as_deref() != Some(newest.as_str()) {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        },
    )
    .await;
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

/// Members of a hosted room seal every Chat write, so the node reads the
/// voice channel a member opens through the keys it stewards: a member's
/// sealed write never leaves a room's voice channels without calls.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the relay stays live until its consuming shutdown"
)]
async fn a_member_opens_a_call_in_a_chat_its_members_seal() {
    const CHAT: &str = "so.tilo.chat";
    const LOBBY: &str = "lobby";
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::cloud_enrollment::host_capabilities());
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().clone();
    let room = rooms.create("Lounge").await.unwrap().room;
    let (member_key, member_cert) = member_device(&rooms, &room, (&key, &cert), 3).await;
    rooms
        .author_room_record(&room, &cert, (CHAT, LOBBY), |doc| {
            let channels = doc
                .put_object(ROOT, "channels", automerge::ObjType::Map)
                .map_err(|e| e.to_string())?;
            let entry = doc
                .put_object(&channels, "voice", automerge::ObjType::Map)
                .map_err(|e| e.to_string())?;
            let voice = doc
                .put_object(&entry, "v", automerge::ObjType::Map)
                .map_err(|e| e.to_string())?;
            doc.put(&voice, "kind", "voice")
                .map_err(|e| e.to_string())?;
            doc.put(&voice, "workspace", "main")
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let member = SealingMember::join(&rooms, &room, (CHAT, LOBBY), member_key).await;
    member
        .writes(&rooms, &room, |document| {
            document.put(ROOT, "topic", "hello").unwrap();
            document.commit_edit("fixture", "topic");
        })
        .await;
    let state = rooms.room(&room).await.unwrap();
    assert!(
        state
            .lock()
            .await
            .peer()
            .publication()
            .is_sealed_record(CHAT, LOBBY),
        "the member's write is sealed"
    );
    drop(state);

    let open = tilo_relay_protocol::voice::CallOperation::Open {
        app_id: CHAT.into(),
        record_id: LOBBY.into(),
        channel_id: "voice".into(),
    };
    let call = rooms
        .voice_call_as(&room, &member_cert, &open)
        .await
        .expect("a member opens the voice channel's call");
    assert!(call.active);
    let peer = tilo_relay_protocol::AuthenticatedPeer {
        cert: member_cert.clone(),
        user_public_key: member_cert.user_public_key,
        device_public_key: member_cert.device_public_key,
        role: tilo_relay_protocol::PeerRole::Device,
    };
    let (outbound, _inbox) = tokio::sync::mpsc::channel(4);
    let calls = rooms
        .voice_calls(&room, &peer, outbound)
        .await
        .expect("the node still reads the room's calls");
    assert!(calls.iter().any(|listed| listed.state.call == call.call));
    drop(rooms);
    relay.shutdown().await;
}

/// What the relay retains of `record`, once it retains a write `wanted`.
async fn retained_once(
    relay: &crate::rooms::fixtures::TestHost,
    workspace_id: u64,
    record: &str,
    wanted: impl Fn(&SignedOp) -> bool + Send + Sync,
) -> Vec<SignedOp> {
    tilo_test_env::within("the relay retains the record", async {
        loop {
            let held = retained_writes(relay, workspace_id, record).await;
            if held.iter().any(&wanted) {
                break held;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
}

/// A node that stewards its room's channels opens a record's channel
/// before the record's first write: nothing of the record is ever retained
/// in the clear, and the room reads it through the channel's keys.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_record_of_a_stewarded_room_is_sealed_from_its_first_write() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::cloud_enrollment::host_capabilities());
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().clone();
    let created = rooms.create("Lounge").await.unwrap();
    let (room, workspace_id) = (created.room, created.workspace_id);
    rooms
        .author_room_record(&room, &cert, (APP, RECORD), |doc| {
            doc.put(ROOT, "title", "Welcome").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let held = retained_once(&relay, workspace_id, RECORD, |op| op.encryption.is_some()).await;
    assert!(held.iter().all(holds_nothing_in_the_clear));
    assert_eq!(readable(&rooms, &room).await.as_deref(), Some("Welcome"));
    drop(rooms);
    relay.shutdown().await;
}

/// A record its room wrote in the clear before the node stewarded its
/// channel is sealed once its operator decided it: its channel is opened, the
/// relay retains one sealed snapshot in place of the writes in the clear,
/// and a member with the channel's keys reads every change of that history
/// from it, with its author and sequence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "TestHost::shutdown consumes the host after the assertions"
)]
async fn a_record_written_in_the_clear_is_sealed_with_its_whole_history() {
    const LEGACY: &str = "legacy";
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::cloud_enrollment::host_capabilities());
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().clone();
    let created = rooms.create("Lounge").await.unwrap();
    let (room, workspace_id) = (created.room, created.workspace_id);
    let (member_key, _) = member_device(&rooms, &room, (&key, &cert), 3).await;
    // Two authors' writes, as the room's host kept them in the clear before
    // it stewarded the record's channel.
    let mut written = automerge::AutoCommit::new();
    written.put(ROOT, "title", "before").unwrap();
    written.commit_edit("fixture", "title");
    let mut other = written.fork();
    other.put(ROOT, "by", "another author").unwrap();
    other.commit_edit("fixture", "by");
    written.put(ROOT, "title", "the channel").unwrap();
    written.commit_edit("fixture", "title");
    written.merge_incoming(&mut other).unwrap();
    {
        let state = rooms.room(&room).await.unwrap();
        let mut governed = state.lock().await;
        let mut candidate = governed.peer().clone();
        let now = SystemTime::now();
        for bytes in [automerge::AutoCommit::new().save(), written.save()] {
            candidate
                .publication_mut()
                .publish_document(APP, LEGACY, bytes, &key, now)
                .unwrap();
        }
        let collaboration = governed.collaboration().clone();
        governed
            .commit(candidate, &key, now, |_, _| Ok(collaboration))
            .unwrap();
        rooms.settle(&mut governed).await.unwrap();
    }
    retained_once(&relay, workspace_id, LEGACY, |op| {
        !holds_nothing_in_the_clear(op)
    })
    .await;

    // The host's operator decided to seal what its rooms hold in the clear.
    rooms.reseal_existing();
    relay.compactor().reseal_existing();
    rooms.reseal_stewarded(relay.compactor()).await;

    let held = retained_writes(&relay, workspace_id, LEGACY).await;
    assert!(held.iter().all(|op| op.encryption.is_some()));
    rooms.reseal_stewarded(relay.compactor()).await;
    assert_eq!(retained_writes(&relay, workspace_id, LEGACY).await, held);
    let member = SealingMember::join(&rooms, &room, (APP, LEGACY), member_key).await;
    let mut read = automerge::AutoCommit::new();
    for op in &held {
        member.open_into(op, &mut read);
    }
    let changes =
        |doc: &mut automerge::AutoCommit| -> std::collections::HashSet<(automerge::ActorId, u64)> {
            doc.get_changes(&[])
                .iter()
                .map(|change| (change.actor_id().clone(), change.seq()))
                .collect()
        };
    assert!(changes(&mut written).is_subset(&changes(&mut read)));
    assert_eq!(title(&read.save()).as_deref(), Some("the channel"));
    drop(rooms);
    relay.shutdown().await;
}
