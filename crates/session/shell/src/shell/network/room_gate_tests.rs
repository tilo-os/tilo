use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use super::ShellNetwork;
use super::room_gate::RoomAction;
use crate::network::protocol::NeighbourMsg;
use tilo_action_policy::{ActionKind, TreePath};

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
        ChannelId, Ed25519PublicKey, MlsAccessRequestV1, MlsGovernanceAction, MlsGovernanceFrame,
        PeerPresence, PeerPresencePayload, Signature,
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
                seq: 0,
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
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
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
                frame: Box::new(MlsGovernanceFrame {
                    version: 2,
                    channel,
                    previous_frame_digest: [7; 32],
                    epoch: 1,
                    action: MlsGovernanceAction::Commit {
                        removed: vec![Ed25519PublicKey::from_bytes([8; 32])],
                        added: Vec::new(),
                        commit: Vec::new(),
                        joining: None,
                        history: None,
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
    assert_eq!(table.len(), 25, "every NeighbourMsg variant has an entry");
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
