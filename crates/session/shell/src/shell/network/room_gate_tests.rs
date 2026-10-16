//! The room gate decides every wire variant explicitly, and a document op
//! from a member who may only read is refused at the gate itself.

use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use super::{ShellNetwork, room_topic_id, shell_network_for_test};
use crate::network::protocol::NeighbourMsg;
use crate::shell::state::{RoomId, WorkspaceTree};
use tilo_action_policy::{ActionKind, TreePath};

use super::room_gate::RoomAction;
use tilo_lock::MutexRecover as _;

/// Reading the room.
fn read() -> RoomAction {
    (TreePath::root(), ActionKind::Read)
}

/// Changing the room.
fn change() -> RoomAction {
    (TreePath::root(), ActionKind::Undoable)
}

/// The least change of a document: a comment.
fn comment() -> RoomAction {
    (
        TreePath::root().field(tilo_action_policy::roles::COMMENTS_FIELD),
        ActionKind::Undoable,
    )
}

const ROOM: &str = "6b3f9a10-0000-4000-8000-00000000000c";

fn cert(user: [u8; 32], device: [u8; 32]) -> DeviceCert {
    DeviceCert {
        user_public_key: user,
        device_public_key: device,
        device_label: "Laptop".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    }
}

fn envelope() -> Box<collab_types::SignedOp> {
    Box::new(collab_types::SignedOp::new_unsigned(
        b"change".to_vec(),
        collab_types::document_artifact_id("so.tilo.notes", ROOM, "notes"),
        collab_types::Ed25519PublicKey::from_bytes([2; 32]),
        collab_types::document_attestation_id("so.tilo.notes", ROOM, "notes"),
        collab_types::OpKind::Change,
        std::time::SystemTime::UNIX_EPOCH,
        1,
    ))
}

/// One message of every variant with the action the gate must decide it as.
/// The gate's own match is exhaustive; this table pins each decision.
#[allow(
    clippy::too_many_lines,
    reason = "one entry per wire variant keeps the whole admission table in one place"
)]
fn every_variant() -> Vec<(NeighbourMsg, Option<RoomAction>)> {
    use collab_types::{
        ChannelId, Ed25519PublicKey, MlsAccessRequestV1, MlsGovernanceAckV1, MlsGovernanceAction,
        MlsGovernanceFrameV1, PeerPresence, PeerPresencePayload, Signature,
    };
    let channel = ChannelId(uuid::Uuid::from_bytes([0x55; 16]));
    let artifact = collab_types::document_artifact_id("so.tilo.notes", ROOM, "notes");
    let device_cert = || Box::new(cert([1; 32], [2; 32]));
    vec![
        (
            NeighbourMsg::SyncReq {
                since: Vec::new(),
                room: ROOM.into(),
                invite_id: None,
                nonce: 0,
            },
            Some(read()),
        ),
        (
            NeighbourMsg::SyncResp {
                changes: Vec::new(),
                room: ROOM.into(),
            },
            Some(change()),
        ),
        (
            NeighbourMsg::PresenceMoved {
                peer: 1,
                user: "ada".into(),
                key: "cursor".into(),
                point: (0.0, 0.0),
                ts: 0,
                colour: None,
            },
            Some(read()),
        ),
        (
            NeighbourMsg::PresenceCleared {
                peer: 1,
                key: "cursor".into(),
            },
            Some(read()),
        ),
        (
            NeighbourMsg::SurfaceFrame {
                surface: 7,
                source_peer: 1,
                encoded_h264: Vec::new(),
                ts: 0,
            },
            Some(change()),
        ),
        (
            NeighbourMsg::SurfaceStreamStart {
                surface: 7,
                source_peer: 1,
                width: 1,
                height: 1,
                codec: "h264".into(),
                ts: 0,
                sender_monotonic_ns_at_send: 0,
                colorimetry: None,
                hdr_static_metadata: None,
                bit_depth: 0,
                app_id: String::new(),
                tab: 0,
            },
            Some(change()),
        ),
        (
            NeighbourMsg::SurfaceStreamEnd {
                surface: 7,
                source_peer: 1,
                ts: 0,
            },
            Some(change()),
        ),
        (
            NeighbourMsg::RemoteInput {
                peer: 1,
                surface: 7,
                event: Vec::new(),
            },
            Some(change()),
        ),
        (
            NeighbourMsg::PermissionRequest {
                peer: 1,
                capability: "workspace-control:7:granted".into(),
            },
            None,
        ),
        (
            NeighbourMsg::PermissionGrant {
                peer: 1,
                capability: "workspace-control:7:granted".into(),
            },
            Some(change()),
        ),
        (NeighbourMsg::Heartbeat { peer: 1, ts: 0 }, Some(read())),
        (
            NeighbourMsg::IdentityHello {
                cert: device_cert(),
                revocations: Vec::new(),
            },
            None,
        ),
        (
            NeighbourMsg::AppDocument {
                peer: 1,
                app_id: "so.tilo.notes".into(),
                cluster_id: ROOM.into(),
                record_id: "notes".into(),
                envelope: envelope(),
            },
            Some(comment()),
        ),
        (
            NeighbourMsg::AppPresenceV2 {
                peer: 1,
                app_id: "so.tilo.notes".into(),
                cluster_id: ROOM.into(),
                presence: PeerPresence {
                    key: vec![1],
                    payload: PeerPresencePayload::Selection { start: 0, end: 1 },
                },
                colour: None,
            },
            Some(read()),
        ),
        (
            NeighbourMsg::ScrollbackRequest {
                artifact,
                before_seq: 1,
                max: 1,
                requester: 1,
            },
            Some(read()),
        ),
        (
            NeighbourMsg::ScrollbackResponse {
                artifact,
                ops: vec![envelope()],
                responder: 1,
            },
            Some(read()),
        ),
        (
            NeighbourMsg::IdentityAnnouncement {
                room: ROOM.into(),
                device_cert: device_cert(),
            },
            None,
        ),
        (
            NeighbourMsg::IdentityAnnouncementV2 {
                room: ROOM.into(),
                device_cert: device_cert(),
                revocations: Vec::new(),
            },
            Some(read()),
        ),
        (
            NeighbourMsg::PersonalContinuityRequest {
                user_public_key: [1; 32],
                requester: 1,
            },
            None,
        ),
        (
            NeighbourMsg::PersonalContinuityCheckpoint {
                user_public_key: [1; 32],
                publisher: 1,
                snapshot: Vec::new(),
            },
            None,
        ),
        (
            NeighbourMsg::RemoteAction { frame: Vec::new() },
            Some(read()),
        ),
        (
            NeighbourMsg::MediaCapabilities {
                peer: 1,
                hardware_decode: Vec::new(),
                software_decode: Vec::new(),
                hardware_encode: Vec::new(),
                software_encode: Vec::new(),
            },
            None,
        ),
        (
            NeighbourMsg::MlsGovernance {
                room: ROOM.into(),
                frame: Box::new(MlsGovernanceFrameV1 {
                    version: 1,
                    transaction_id: [6; 32],
                    channel,
                    membership_revision: 1,
                    previous_frame_digest: [7; 32],
                    epoch: 1,
                    action: MlsGovernanceAction::Remove {
                        removed: Ed25519PublicKey::from_bytes([8; 32]),
                        commit: Vec::new(),
                    },
                    signed_by: Ed25519PublicKey::from_bytes([9; 32]),
                    signature: Signature::from_bytes([0xAB; 64]),
                }),
                delivery: 0,
            },
            Some(read()),
        ),
        (
            NeighbourMsg::MlsAccessRequest {
                room: ROOM.into(),
                request: Box::new(MlsAccessRequestV1 {
                    version: 1,
                    channel,
                    device: Ed25519PublicKey::from_bytes([2; 32]),
                    key_package: Vec::new(),
                    issued_at: 0,
                    signature: Signature::from_bytes([0xCD; 64]),
                }),
                delivery: 0,
            },
            Some(read()),
        ),
        (
            NeighbourMsg::MlsGovernanceAck {
                room: ROOM.into(),
                ack: Box::new(MlsGovernanceAckV1 {
                    version: 1,
                    channel,
                    device: Ed25519PublicKey::from_bytes([2; 32]),
                    through_digest: [0xEE; 32],
                    signature: Signature::from_bytes([0xEF; 64]),
                }),
                delivery: 0,
            },
            Some(read()),
        ),
        (
            NeighbourMsg::PersonalWorkspaces {
                user_public_key: [1; 32],
                update: Box::new(tilo_user_identity::DirectoryUpdate {
                    author_device_public_key: [2; 32],
                    authored_at_ns: 0,
                    delta: Vec::new(),
                    signature: [3; 64],
                }),
            },
            None,
        ),
        (
            NeighbourMsg::RecordContentWant {
                requester: 1,
                app_id: "so.tilo.notes".into(),
                cluster_id: ROOM.into(),
                hash: "ab".into(),
            },
            Some(read()),
        ),
        (
            NeighbourMsg::RecordContentPiece {
                responder: 1,
                requester: 2,
                app_id: "so.tilo.notes".into(),
                cluster_id: ROOM.into(),
                hash: "ab".into(),
                total: 1,
                offset: 0,
                bytes: vec![1],
            },
            Some(read()),
        ),
    ]
}

#[test]
fn required_room_action_is_explicit_for_every_variant() {
    let table = every_variant();
    let covered: std::collections::HashSet<_> = table
        .iter()
        .map(|(message, _)| std::mem::discriminant(message))
        .collect();
    assert_eq!(
        covered.len(),
        table.len(),
        "one message per variant, none repeated"
    );
    assert_eq!(table.len(), 28, "every NeighbourMsg variant has an entry");
    for (message, expected) in &table {
        assert_eq!(
            ShellNetwork::required_room_action(message),
            *expected,
            "{message:?}"
        );
        // A retired generation is never admitted as room traffic.
        if message.is_retired() {
            assert_eq!(*expected, None, "{message:?}");
        }
    }
}

/// Everything written through `tracing` while a test runs.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn take(&self) -> String {
        let mut bytes = self.0.lock_recover();
        String::from_utf8_lossy(&std::mem::take(&mut *bytes)).into_owned()
    }
}

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock_recover().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

const GATE_REFUSAL: &str = "dropping collaboration message without workspace authority";

/// A member the room admits only to read is refused a document op by the
/// room gate itself, on the production inbound path; the same member's
/// read-side traffic passes the gate, and a commenter's document op does too.
#[test]
fn viewer_app_document_is_dropped_at_the_gate() {
    // Accepting an identity names the device from the room's configured
    // identity; keep tests that repoint the configuration out of the way.
    let _env = tilo_test_env::lock();
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _logs = tracing::subscriber::set_default(subscriber);

    let user_root = SigningKey::from_bytes(&[71; 32]);
    let device = iroh::SecretKey::from_bytes(&[72; 32]);
    let mut device_cert = cert(
        user_root.verifying_key().to_bytes(),
        *device.public().as_bytes(),
    );
    sign_device_cert(&mut device_cert, &user_root).expect("certificate");
    let collab_types::caps::Principal::Person(person) =
        collab_types::caps::Principal::person_from_key(
            collab_types::crypto::Ed25519PublicKey::from_bytes(device_cert.user_public_key),
        )
    else {
        unreachable!()
    };

    let room = RoomId::parse(ROOM).expect("room id");
    let mut workspace =
        crate::shell::state::Workspace::new(crate::shell::state::WorkspaceId(17), "Shared");
    workspace.set_room(room);
    workspace.set_owner(crate::shell::state::local_user_id_for_room(&room));
    workspace.admit(person.clone(), tilo_action_policy::Role::Viewer);
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    let (mut net, _commands) = shell_network_for_test(tree.shared(), 99);
    net.require_peer_identity = true;

    let ws = room_topic_id(room);
    let peer = device.public();
    let handle = crate::network::behaviour::peer_liveness_handle(&peer);
    net.dispatch_neighbour_msg(
        ws,
        peer,
        None,
        false,
        NeighbourMsg::IdentityHello {
            cert: Box::new(device_cert.clone()),
            revocations: Vec::new(),
        },
    );
    assert!(
        net.peer_identities
            .get(&crate::shell::state::PeerOrigin::peer(&peer))
            .is_some(),
        "the viewer's device is authenticated"
    );
    let document = || NeighbourMsg::AppDocument {
        peer: handle,
        app_id: "so.tilo.notes".into(),
        cluster_id: ROOM.into(),
        record_id: "notes".into(),
        envelope: envelope(),
    };
    captured.take();

    // Read-side traffic from the viewer passes the gate: the refusal below
    // is the document op's own requirement, not a missing membership.
    net.dispatch_neighbour_msg(
        ws,
        peer,
        None,
        false,
        NeighbourMsg::Heartbeat {
            peer: handle,
            ts: 1,
        },
    );
    let logs = captured.take();
    assert!(!logs.contains(GATE_REFUSAL), "{logs}");

    net.dispatch_neighbour_msg(ws, peer, None, false, document());
    let logs = captured.take();
    assert!(logs.contains(GATE_REFUSAL), "{logs}");
    assert!(logs.contains(r#"Name(Field, "comments")"#), "{logs}");

    let tree = net.replica.lock().expect("replica").tree.clone();
    tree.write()
        .desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.room == room)
        .expect("workspace")
        .admit(person, tilo_action_policy::Role::Commenter);
    net.dispatch_neighbour_msg(ws, peer, None, false, document());
    let logs = captured.take();
    assert!(!logs.contains(GATE_REFUSAL), "{logs}");
}
