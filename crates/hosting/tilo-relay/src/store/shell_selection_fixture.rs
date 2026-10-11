//! Builds the shell-selection scenario the store, peer and persistence tests
//! replay, at test time.
//!
//! A governor owns a room with a chat, a notes page and a Writer document and
//! admits a second device of the same person. It publishes a full
//! checkpoint (the baseline). The other device then writes one signed change
//! to the Writer document; the governor admits it and publishes the
//! route-selective tail: the room's coverage delta and the Writer's covered
//! snapshot. A room-only rename follows as one more room delta. The governor
//! finally publishes the next full checkpoint. Every identity comes from a
//! fixed seed here, and the scenario is built only through the ordinary
//! `StatePeer` APIs, so it follows whatever they currently sign and write.
//!
//! Layout written by [`write`]: `manifest.json` (room, authority, certificates,
//! admission time, final rows, and per batch each message's file, retention
//! kind, channel, size and BLAKE3 hash), `baseline/`, `tail/` and
//! `checkpoint2/` holding the postcard `NeighbourMsg` bodies, and the peer
//! exports `baseline-peer.postcard` (the governor before the foreign change)
//! and `final-peer.postcard`, plus the foreign author's exact signed change in
//! `original-foreign.postcard`.
use super::*;
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use tilo_peer_state::{NeighbourMsg, StatePeer, TransportOrigin, transport::Retention};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};
use web_time::{Duration, SystemTime};

const ROOM: &str = "a85778ff-9b1d-47f1-8baf-b3aa8b22ddfd";
const BASE_NS: u64 = 1_790_000_000_000_000_000;
/// The admission time the tests replay at.
const NOW_OFFSET_SECS: u64 = 10;
const CHAT: (&str, &str) = ("so.tilo.chat", "");
const NOTES: (&str, &str) = ("so.tilo.notes", "tab:f407000000000005");
const WRITER: (&str, &str) = ("so.tilo.word-processor", "tab:0000000000000001");

fn at(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_nanos(BASE_NS))
        .and_then(|time| time.checked_add(Duration::from_secs(seconds)))
        .unwrap()
}

fn certificate(
    root: &SigningKey,
    device: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: BASE_NS,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
    cert
}

fn document(actor: u8, fields: &[(&str, &str)]) -> Vec<u8> {
    let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(vec![actor; 16]));
    for (key, value) in fields {
        doc.put(ROOT, *key, *value).unwrap();
    }
    doc.commit();
    doc.save()
}

/// How a route-selective publication is retained: the room's changes since
/// the last publication as a delta, each touched route's covered snapshot as
/// a checkpoint.
fn kind_of(message: &NeighbourMsg) -> Retention {
    if matches!(message, NeighbourMsg::SyncResp { .. }) {
        Retention::Delta
    } else {
        Retention::Checkpoint
    }
}

/// One batch of the governor's publications with how each is retained, as
/// written to disk.
pub(crate) struct Batch(Vec<(NeighbourMsg, Retention)>);

impl Batch {
    /// A full checkpoint: every message is retained as a checkpoint.
    fn checkpoint(messages: Vec<NeighbourMsg>) -> Self {
        Self(
            messages
                .into_iter()
                .map(|message| (message, Retention::Checkpoint))
                .collect(),
        )
    }

    /// A route-selective publication.
    fn selective(messages: Vec<NeighbourMsg>) -> Self {
        Self(
            messages
                .into_iter()
                .map(|message| {
                    let kind = kind_of(&message);
                    (message, kind)
                })
                .collect(),
        )
    }
}

/// The generated scenario, before it is written to disk.
pub(crate) struct ShellSelectionFixture {
    pub(crate) room: String,
    pub(crate) authority: DeviceCert,
    pub(crate) certificates: Vec<DeviceCert>,
    pub(crate) baseline: Batch,
    pub(crate) tail: Batch,
    pub(crate) checkpoint2: Batch,
    pub(crate) rows: Vec<tilo_peer_state::DocumentRow>,
    pub(crate) baseline_peer: Vec<u8>,
    pub(crate) final_peer: Vec<u8>,
    pub(crate) original_foreign: Vec<u8>,
}

/// Build the scenario. See the module docs.
#[expect(
    clippy::too_many_lines,
    reason = "one scenario: the governor's publications and the foreign change stay together"
)]
pub(crate) fn generate() -> ShellSelectionFixture {
    let person = SigningKey::from_bytes(&[0x61; 32]);
    let key = SigningKey::from_bytes(&[0x62; 32]);
    let foreign_key = SigningKey::from_bytes(&[0x63; 32]);
    let authority = certificate(
        &person,
        &key,
        "Governing device",
        DeviceCapabilities::primary(),
    );
    let foreign = certificate(
        &person,
        &foreign_key,
        "foreign fixture author",
        DeviceCapabilities::member(),
    );
    let mut owner = StatePeer::new(
        ROOM,
        authority.clone(),
        vec![authority.clone()],
        RevocationList::new(),
        at(0),
    )
    .unwrap();
    let seed =
        |route: (&str, &str), actor: u8, fields: &[(&str, &str)]| tilo_peer_state::SeedDocument {
            app_id: route.0.into(),
            record_id: route.1.into(),
            kind: "fixture".into(),
            bytes: document(actor, fields),
        };
    owner
        .install_seed(
            tilo_peer_state::SeedState {
                room_changes: tilo_peer_state::seed_room(
                    ROOM,
                    &authority,
                    std::slice::from_ref(&authority),
                )
                .unwrap(),
                documents: vec![
                    seed(CHAT, 1, &[("title", "Chat")]),
                    seed(NOTES, 2, &[("title", "Notes")]),
                    seed(WRITER, 3, &[("title", "Draft")]),
                ],
            },
            &key,
            at(0),
        )
        .unwrap();
    assert!(
        owner
            .admit_certificate(foreign.clone(), &key, at(1))
            .unwrap()
    );

    let baseline = owner.durable_bootstrap_messages(&key, at(2), 1).unwrap();
    let baseline_peer = owner.durable().unwrap();

    // The other device writes one signed change to the Writer document.
    let mut sender = owner.clone();
    let mut doc = AutoCommit::load(&sender.document(WRITER.0, WRITER.1).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(foreign.device_public_key.to_vec()));
    let heads = doc.get_heads();
    doc.put(ROOT, "foreign_once", "original signed change")
        .unwrap();
    doc.commit_edit("fixture", "foreign_once");
    let original = sender
        .publish_incremental(
            WRITER.0,
            WRITER.1,
            doc.save_after(&heads),
            &foreign_key,
            at(3),
        )
        .unwrap();
    let original_foreign = postcard::to_allocvec(&original).unwrap();

    let before = owner.clone();
    owner
        .receive_from(
            TransportOrigin::RelayDelta(foreign.device_public_key),
            original.clone(),
            at(4),
        )
        .unwrap();
    let mut tail = owner
        .durable_mutation_messages(&before, &key, at(4), 2)
        .unwrap();
    assert_eq!(
        tail.iter().map(kind_of).collect::<Vec<_>>(),
        [Retention::Delta, Retention::Checkpoint],
        "the foreign change publishes the room's coverage and the Writer's snapshot"
    );
    assert!(owner.covers(&original));

    // A room-only rename: one more room delta.
    let before = owner.clone();
    let mut room = owner.admitted_room().unwrap().clone();
    let heads = room.get_heads();
    room.put(ROOT, "name", "selectively renamed room").unwrap();
    room.commit();
    let changes = room
        .get_changes(&heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    owner
        .receive_from(
            TransportOrigin::RelayDelta(authority.device_public_key),
            NeighbourMsg::SyncResp {
                room: ROOM.into(),
                changes,
            },
            at(5),
        )
        .unwrap();
    let rename = owner
        .durable_mutation_messages(&before, &key, at(5), 3)
        .unwrap();
    assert_eq!(
        rename.iter().map(kind_of).collect::<Vec<_>>(),
        [Retention::Delta]
    );
    tail.extend(rename);

    let rows = owner.rows();
    let checkpoint2 = owner.durable_bootstrap_messages(&key, at(6), 4).unwrap();
    ShellSelectionFixture {
        room: ROOM.into(),
        certificates: owner.certificates(),
        authority,
        baseline: Batch::checkpoint(baseline),
        tail: Batch::selective(tail),
        checkpoint2: Batch::checkpoint(checkpoint2),
        rows,
        baseline_peer,
        final_peer: owner.durable().unwrap(),
        original_foreign,
    }
}

fn write_batch(dir: &Path, name: &str, batch: &Batch) -> serde_json::Value {
    fs::create_dir_all(dir.join(name)).unwrap();
    serde_json::Value::Array(
        batch
            .0
            .iter()
            .enumerate()
            .map(|(index, (message, kind))| {
                let kind = *kind;
                let file = format!("{name}/{index:03}.postcard");
                let bytes = postcard::to_allocvec(message).unwrap();
                fs::write(dir.join(&file), &bytes).unwrap();
                let (channel, actual) =
                    tilo_peer_state::transport::retention(message, kind == Retention::Checkpoint)
                        .unwrap();
                assert_eq!(actual, kind);
                serde_json::json!({
                    "file": file,
                    "kind": format!("{kind:?}"),
                    "channel": hex::encode(channel),
                    "bytes": bytes.len(),
                    "blake3": blake3::hash(&bytes).to_hex().to_string(),
                })
            })
            .collect(),
    )
}

/// Write the scenario into `dir` in the layout the tests read.
pub(crate) fn write(fixture: &ShellSelectionFixture, dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    let manifest = serde_json::json!({
        "version": 1,
        "room": fixture.room,
        "authority": fixture.authority,
        "certificates": fixture.certificates,
        "now_ns": BASE_NS + NOW_OFFSET_SECS * 1_000_000_000,
        "rows": fixture.rows,
        "baseline": write_batch(dir, "baseline", &fixture.baseline),
        "tail": write_batch(dir, "tail", &fixture.tail),
        "checkpoint2": write_batch(dir, "checkpoint2", &fixture.checkpoint2),
    });
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.join("original-foreign.postcard"),
        &fixture.original_foreign,
    )
    .unwrap();
    fs::write(dir.join("baseline-peer.postcard"), &fixture.baseline_peer).unwrap();
    fs::write(dir.join("final-peer.postcard"), &fixture.final_peer).unwrap();
}
