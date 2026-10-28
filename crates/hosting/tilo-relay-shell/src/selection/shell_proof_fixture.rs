//! Builds the shell-proof scenario the tests replay, at test time.
//!
//! Every identity is derived from a fixed seed in this file and every
//! signature and timestamp is fixed. The scenario is built only through the
//! ordinary `StatePeer` publishing APIs and the relay's own `WorkspaceStore`
//! retention, so it follows whatever those currently write.
//!
//! Layout written by [`write`]: `frames/0000.postcard`… are the delivered
//! `CoeditPayload` bodies (decode the wrapper before `NeighbourMsg`);
//! `retained/` is the relay's retained directory after it took in every
//! publication after generation 1 (`.delta` files are complete
//! four-byte-length-prefixed `Envelope` frames with zero outer signatures, a
//! trusted test import); `initial-peer.postcard` is the receiver's export
//! after generation 1; `failure.json` names the governor, certificates, room
//! and admission time, the frame count, the room-checkpoint frame and the
//! superseded route.
use super::*;
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use neighbour_wire::{NeighbourMsg, Retention};
use tilo_peer_state::{StatePeer, TransportOrigin};
use tilo_relay::store::{RETAINED_DIR, WorkspaceStore};
use tilo_relay::transport::Envelope;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};
use web_time::{Duration, SystemTime};

/// Fixed room of the scenario.
const ROOM: &str = "5e11f00d-7110-4000-8000-000000000001";
/// Fixed base time; every signature and admission is relative to it.
const BASE_NS: u64 = 1_800_000_000_000_000_000;
/// The admission time the fixture tests replay at.
const NOW_OFFSET_SECS: u64 = 120;
/// The route whose delayed checkpoint is superseded by governance.
const NOTES: (&str, &str) = ("so.tilo.notes", "tab:0000000000000004");
const WORD_PROCESSOR: (&str, &str) = ("so.tilo.word-processor", "tab:0000000000000001");
const SHEETS: (&str, &str) = ("so.tilo.sheets", "tab:0000000000000002");
const CHAT: (&str, &str) = ("so.tilo.chat", "");

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

/// A document authored under a fixed actor, so its bytes are reproducible.
fn document(actor: u8, fields: &[(&str, &str)]) -> Vec<u8> {
    let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(vec![actor; 16]));
    for (key, value) in fields {
        doc.put(ROOT, *key, *value).unwrap();
    }
    doc.commit();
    doc.save()
}

/// The governor's incremental edit of one route.
fn edit(
    owner: &mut StatePeer,
    key: &SigningKey,
    (app, record): (&str, &str),
    field: &str,
    value: &str,
    now: SystemTime,
) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&owner.document(app, record).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(ROOT, field, value).unwrap();
    doc.commit_edit("fixture", field);
    owner
        .publication_mut()
        .publish_incremental(app, record, doc.save_after(&heads), key, now)
        .unwrap()
}

fn route_of(message: &NeighbourMsg) -> Option<(&str, &str)> {
    match message {
        NeighbourMsg::AppDocument {
            app_id, record_id, ..
        } => Some((app_id, record_id)),
        _ => None,
    }
}

fn sequence_of(message: &NeighbourMsg) -> u64 {
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        panic!("not a document")
    };
    envelope.seq
}

/// One governor publication, as the owner marked it for retention.
#[derive(Clone)]
struct Publication {
    message: NeighbourMsg,
    kind: Retention,
    at: SystemTime,
}

impl Publication {
    fn payload(&self) -> CoeditPayload {
        let (channel, kind) =
            neighbour_wire::retention(&self.message, self.kind == Retention::Checkpoint).unwrap();
        assert_eq!(kind, self.kind);
        let body = postcard::to_allocvec(&self.message).unwrap();
        match kind {
            Retention::Checkpoint => CoeditPayload::Checkpoint { channel, body },
            Retention::Delta => CoeditPayload::RetainedDelta { channel, body },
        }
    }
}

fn bootstrap(
    owner: &mut StatePeer,
    key: &SigningKey,
    generation: u64,
    now: SystemTime,
) -> Vec<Publication> {
    owner
        .publication_mut()
        .durable_bootstrap_messages(key, now, generation)
        .unwrap()
        .into_iter()
        .map(|message| Publication {
            message,
            kind: Retention::Checkpoint,
            at: now,
        })
        .collect()
}

fn find<'a>(batch: &'a [Publication], route: Option<(&str, &str)>) -> &'a Publication {
    batch
        .iter()
        .find(|publication| route_of(&publication.message) == route)
        .unwrap()
}

/// The generated fixture, before it is written to disk.
pub(crate) struct ShellProofFixture {
    /// Encoded `CoeditPayload` bodies in delivery order.
    pub(crate) frames: Vec<Vec<u8>>,
    /// The receiver's durable export before the frames are delivered.
    pub(crate) initial_peer: Vec<u8>,
    /// `failure.json`.
    pub(crate) metadata: serde_json::Value,
    /// Keeps the relay store whose `retained/` directory is the fixture's alive.
    pub(crate) _store: tempfile::TempDir,
    /// The relay store root inside `store`.
    pub(crate) store_root: PathBuf,
}

/// Build the scenario the shell-proof tests assert.
///
/// The governor seeds four routes and publishes durable bootstraps at
/// generations 1 to 6, with incremental edits between generations 3 and 4.
/// Generation 3 covers the notes route at sequence 4 and generation 6 covers
/// it at sequence 7 with the identical payload, because notes never changes.
/// The receiver's initial state is generation 1. The delivered frames carry
/// generation 3, the edits and generation 6, then the late notes sequence-4
/// checkpoint, which governance has already superseded. The relay retained
/// every publication after generation 1.
#[expect(
    clippy::too_many_lines,
    reason = "one scenario: the governor's publications, the delivered frames and the retained store stay together"
)]
pub(crate) fn generate() -> ShellProofFixture {
    let root = SigningKey::from_bytes(&[0x51; 32]);
    let governor_key = SigningKey::from_bytes(&[0x52; 32]);
    let host_key = SigningKey::from_bytes(&[0x53; 32]);
    let governor = certificate(
        &root,
        &governor_key,
        "Native workspace peer",
        DeviceCapabilities::primary(),
    );
    let host = certificate(&root, &host_key, "Tilo host", DeviceCapabilities::host());
    let certificates = vec![governor.clone(), host];
    let source = governor.device_public_key;

    let mut owner = StatePeer::new(
        ROOM,
        governor.clone(),
        certificates.clone(),
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
                room_changes: tilo_peer_state::seed_room(ROOM, &governor, &certificates).unwrap(),
                documents: vec![
                    seed(NOTES, 1, &[("title", "Notes"), ("body", "unchanged")]),
                    seed(WORD_PROCESSOR, 2, &[("title", "Draft")]),
                    seed(SHEETS, 3, &[("title", "Budget")]),
                    seed(CHAT, 4, &[("title", "Chat")]),
                ],
            },
            &governor_key,
            at(0),
        )
        .unwrap();

    let mut receiver = StatePeer::new(
        ROOM,
        governor.clone(),
        certificates.clone(),
        RevocationList::new(),
        at(1),
    )
    .unwrap();
    let origin = TransportOrigin::RelayDelta(source);
    for publication in bootstrap(&mut owner, &governor_key, 1, at(1)) {
        let outcome = receiver
            .receive_from(origin, publication.message, at(1))
            .unwrap();
        assert_eq!(outcome.rejected, 0);
    }
    let initial_peer = receiver.durable().unwrap();

    let mut retained = Vec::new();
    retained.extend(bootstrap(&mut owner, &governor_key, 2, at(2)));
    let generation3 = bootstrap(&mut owner, &governor_key, 3, at(3));
    retained.extend(generation3.iter().cloned());
    let edits = [
        edit(
            &mut owner,
            &governor_key,
            WORD_PROCESSOR,
            "body",
            "first paragraph",
            at(4),
        ),
        edit(&mut owner, &governor_key, CHAT, "topic", "planning", at(5)),
    ]
    .map(|message| Publication {
        message,
        kind: Retention::Delta,
        at: at(4),
    });
    retained.extend(edits.iter().cloned());
    retained.extend(bootstrap(&mut owner, &governor_key, 4, at(6)));
    retained.extend(bootstrap(&mut owner, &governor_key, 5, at(7)));
    let generation6 = bootstrap(&mut owner, &governor_key, 6, at(8));
    retained.extend(generation6.iter().cloned());

    let superseded = find(&generation3, Some(NOTES));
    let successor = find(&generation6, Some(NOTES));
    assert_eq!(
        (
            sequence_of(&superseded.message),
            sequence_of(&successor.message)
        ),
        (4, 7)
    );
    let delivered = [
        find(&generation3, None),
        find(&generation3, Some(WORD_PROCESSOR)),
        find(&generation3, Some(CHAT)),
        find(&generation3, Some(SHEETS)),
        &edits[0],
        &edits[1],
        find(&generation6, None),
        find(&generation6, Some(WORD_PROCESSOR)),
        find(&generation6, Some(CHAT)),
        find(&generation6, Some(SHEETS)),
        successor,
        superseded,
    ];
    let room_checkpoint_frame = 6;
    let superseded_frame = delivered.len().checked_sub(1).unwrap();
    let frames = delivered
        .iter()
        .map(|publication| publication.payload().encode().unwrap())
        .collect::<Vec<_>>();

    // The relay's unbound store retains every publication in receipt order,
    // exactly as its authenticated ingress does.
    let store = tempfile::tempdir().unwrap();
    let workspace = tilo_relay::room_workspace_id(ROOM).unwrap();
    let relay = WorkspaceStore::open(store.path(), workspace, true).unwrap();
    for (index, publication) in retained.iter().enumerate() {
        let payload = publication.payload();
        let bytes = tilo_relay::transport::encode(&Envelope::Delta {
            workspace_id: workspace,
            author_device_public_key: source,
            authored_at_ns: u64::try_from(
                publication
                    .at
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
            )
            .unwrap(),
            signature: [0; 64],
            delta: payload.encode().unwrap(),
        })
        .unwrap();
        match payload {
            CoeditPayload::Checkpoint { channel, .. } => {
                relay.retain_checkpoint(&channel, &bytes).unwrap();
            }
            CoeditPayload::RetainedDelta { channel, .. } => {
                relay
                    .retain_delta(
                        &channel,
                        u64::try_from(index).unwrap().checked_add(1).unwrap(),
                        &bytes,
                    )
                    .unwrap();
            }
        }
    }
    let store_root = relay.root().to_owned();

    let metadata = serde_json::json!({
        "room": ROOM,
        "authority": governor,
        "certificates": certificates,
        "now_ns": BASE_NS + NOW_OFFSET_SECS * 1_000_000_000,
        "frames": frames.len(),
        "room_checkpoint_frame": room_checkpoint_frame,
        "superseded": {
            "frame": superseded_frame,
            "app_id": NOTES.0,
            "record_id": NOTES.1,
            "sequence": sequence_of(&superseded.message),
            "governed_sequence": sequence_of(&successor.message),
        },
    });
    ShellProofFixture {
        frames,
        initial_peer,
        metadata,
        _store: store,
        store_root,
    }
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

/// Write the fixture into `dir`, replacing any previous frames and retained
/// directory.
pub(crate) fn write(fixture: &ShellProofFixture, dir: &Path) {
    for stale in ["frames", "retained"] {
        let path = dir.join(stale);
        if path.exists() {
            fs::remove_dir_all(&path).unwrap();
        }
    }
    fs::create_dir_all(dir.join("frames")).unwrap();
    for (index, frame) in fixture.frames.iter().enumerate() {
        fs::write(dir.join(format!("frames/{index:04}.postcard")), frame).unwrap();
    }
    fs::write(dir.join("initial-peer.postcard"), &fixture.initial_peer).unwrap();
    fs::write(
        dir.join("failure.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&fixture.metadata).unwrap()
        ),
    )
    .unwrap();
    copy_tree(
        &fixture.store_root.join(RETAINED_DIR),
        &dir.join(RETAINED_DIR),
    );
}

#[test]
fn shell_proof_fixture_generator_builds_the_asserted_scenario() {
    let fixture = generate();
    let scratch = tempfile::tempdir().unwrap();
    write(&fixture, scratch.path());
    assert_eq!(fixture.frames.len(), 12);
    assert!(scratch.path().join("retained/v2").is_dir());
}
