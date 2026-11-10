//! The workspace gate: which action in a workspace a message from another member is, and whether
//! the workspace's policy lets them do it.
//! One exhaustive match decides the action, with no default arm. Appending a variant to the wire
//! vocabulary therefore fails to compile here until someone states what the new message demands, so
//! a message that changes what members see can never ride on read access because its author forgot
//! this table.
//! The action is then answered by the workspace's own policy
//! ([`tilo_desktop_model::Workspace::answer_at`], the model the broker decides an app's actions
//! with): a member the workspace narrowed to reading keeps sending what reading covers — asking to
//! view a shared window, their presence — and nothing else.

use neighbour_wire::NeighbourMsg;
use tilo_action_policy::{ActionKind, Answer, TreePath, roles::COMMENTS_FIELD};
use tilo_desktop_model::DesktopTree;

/// What a workspace message is: the node below the workspace it acts at, and the kind of action it
/// is.
pub type WorkspaceAction = (TreePath, ActionKind);

/// Whether the workspace `workspace` of `tree` lets `person` do `required`,
/// at `now` (Unix seconds). A desk that holds no such workspace admits
/// nothing: there is no policy here to ask.
pub fn allows(
    tree: &DesktopTree,
    workspace: &str,
    person: &str,
    required: &WorkspaceAction,
    now: u64,
) -> bool {
    let (node, kind) = required;
    tree.workspaces
        .iter()
        .find(|held| held.id().to_string() == workspace)
        .is_some_and(|workspace| {
            workspace.answer_at(person, &workspace.node().join(node), *kind, now) == Answer::Allow
        })
}

/// The action `msg` is, which its sender must be allowed in the workspace it arrived in, or `None`
/// when it is never admitted as a workspace's traffic.
/// Publishing media, injecting input, granting control, writing documents and changing the
/// workspace are active collaboration actions and never ride on read access. `None` covers retired
/// generations and the messages settled before the gate (identity, codec adverts, the person's own
/// list): one of those reaching it means that earlier path was bypassed, and it is refused rather
/// than admitted on some default.
pub fn required_workspace_action(msg: &NeighbourMsg) -> Option<WorkspaceAction> {
    match msg {
        // The workspace's state, published media, injected input and control consent change what
        // other members see or run.
        NeighbourMsg::SyncResp { .. }
        | NeighbourMsg::SurfaceFrame { .. }
        | NeighbourMsg::SurfaceStreamStart { .. }
        | NeighbourMsg::SurfaceStreamEnd { .. }
        | NeighbourMsg::RemoteInput { .. }
        | NeighbourMsg::PermissionGrant { .. } => Some((TreePath::root(), ActionKind::Undoable)),
        // A document op: the least any op is (a comment). What each envelope changes is decided
        // again against the author's standing by the broker.
        NeighbourMsg::AppDocument { .. } => {
            Some((TreePath::root().field(COMMENTS_FIELD), ActionKind::Undoable))
        }
        // Asking for state the sender may read, or answering with content each piece of which is
        // verified on its own (signed ops, content hashes).
        NeighbourMsg::SyncReq { .. }
        | NeighbourMsg::ScrollbackRequest { .. }
        | NeighbourMsg::ScrollbackResponse { .. }
        | NeighbourMsg::RecordContentWant { .. }
        | NeighbourMsg::RecordContentPiece { .. }
        | NeighbourMsg::RecordContentLacks { .. }
        // A viewer asking for a keyframe of a stream it receives; the owner checks it is one.
        | NeighbourMsg::SurfaceKeyframeRequest { .. }
        // A member saying which of the receiving device's windows it views: it may see the
        // workspace's windows, and is streamed them read only.
        | NeighbourMsg::SurfaceViewRequest { .. }
        // Ephemeral liveness of a member who can see the workspace; nothing is written to its
        // state.
        | NeighbourMsg::Heartbeat { .. }
        | NeighbourMsg::LivePresence { .. }
        // The sender's own device certificate, which only the workspace's governor may promote into
        // its governance.
        | NeighbourMsg::IdentityAnnouncementV2 { .. }
        // Each remote action is authorized per call against its own declared permission.
        | NeighbourMsg::RemoteAction { .. }
        // Channel governance is signed by a participant and checked against the membership; access
        // requests are how a reader obtains keys.
        | NeighbourMsg::MlsGovernance { .. }
        | NeighbourMsg::MlsAccessRequest { .. }
        | NeighbourMsg::MlsRoutedAccessRequest { .. } => {
            Some((TreePath::root(), ActionKind::Read))
        }
        // Settled before the gate: identity is how a sender becomes known at all; a codec advert,
        // the person's workspace list and a device's verdicts on its siblings' writes travel only
        // between the person's own devices, never in a workspace.
        NeighbourMsg::IdentityHello { .. }
        | NeighbourMsg::MediaCapabilities { .. }
        | NeighbourMsg::PersonalWorkspaces { .. }
        | NeighbourMsg::PersonalHosts { .. }
        | NeighbourMsg::WriteRefusals { .. }
        // Retired generations, refused where they enter
        // (`NeighbourMsg::is_retired`): presence is a frame on the live plane,
        // not a message here.
        | NeighbourMsg::IdentityAnnouncement { .. }
        | NeighbourMsg::PermissionRequest { .. }
        | NeighbourMsg::RetiredPresenceMoved { .. }
        | NeighbourMsg::RetiredPresenceCleared { .. }
        | NeighbourMsg::RetiredAppPresenceV2 { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert};

    fn read() -> WorkspaceAction {
        (TreePath::root(), ActionKind::Read)
    }

    /// Changing the workspace.
    fn change() -> WorkspaceAction {
        (TreePath::root(), ActionKind::Undoable)
    }

    /// The least change of a document: a comment.
    fn comment() -> WorkspaceAction {
        (TreePath::root().field(COMMENTS_FIELD), ActionKind::Undoable)
    }

    const WORKSPACE: &str = "6b3f9a10-0000-4000-8000-00000000000c";

    fn cert(user: [u8; 32], device: [u8; 32]) -> DeviceCert {
        DeviceCert {
            user_public_key: user,
            device_public_key: device,
            device_label: "Laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        }
    }

    fn envelope() -> Box<collab_types::SignedOp> {
        Box::new(collab_types::SignedOp::new_unsigned(
            b"change".to_vec(),
            collab_types::document_artifact_id("so.tilo.notes", WORKSPACE, "notes"),
            collab_types::Ed25519PublicKey::from_bytes([2; 32]),
            collab_types::document_attestation_id("so.tilo.notes", WORKSPACE, "notes"),
            collab_types::OpKind::Change,
            web_time::SystemTime::UNIX_EPOCH,
            1,
        ))
    }

    /// One message of every variant with the action the gate must decide it as. The gate's own
    /// match is exhaustive; this table pins each decision.
    #[allow(
        clippy::too_many_lines,
        reason = "one entry per wire variant keeps the whole admission table in one place"
    )]
    fn every_variant() -> Vec<(NeighbourMsg, Option<WorkspaceAction>)> {
        use collab_types::{
            ChannelId, Ed25519PublicKey, MlsAccessRequestV1, MlsGovernanceAction,
            MlsGovernanceFrame, PeerPresence, PeerPresencePayload, Signature,
        };
        let channel = ChannelId(uuid::Uuid::from_bytes([0x55; 16]));
        let artifact = collab_types::document_artifact_id("so.tilo.notes", WORKSPACE, "notes");
        let device_cert = || Box::new(cert([1; 32], [2; 32]));
        vec![
            (
                NeighbourMsg::SyncReq {
                    since: Vec::new(),
                    room: WORKSPACE.into(),
                    invite_id: None,
                    nonce: 0,
                },
                Some(read()),
            ),
            (
                NeighbourMsg::SyncResp {
                    changes: Vec::new(),
                    room: WORKSPACE.into(),
                },
                Some(change()),
            ),
            (
                NeighbourMsg::RetiredPresenceMoved {
                    peer: 1,
                    user: "ada".into(),
                    key: "cursor".into(),
                    point: (0.0, 0.0),
                    ts: 0,
                    colour: None,
                },
                None,
            ),
            (
                NeighbourMsg::RetiredPresenceCleared {
                    peer: 1,
                    key: "cursor".into(),
                },
                None,
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
                    cluster_id: WORKSPACE.into(),
                    record_id: "notes".into(),
                    envelope: envelope(),
                },
                Some(comment()),
            ),
            (
                NeighbourMsg::RetiredAppPresenceV2 {
                    peer: 1,
                    app_id: "so.tilo.notes".into(),
                    cluster_id: WORKSPACE.into(),
                    presence: PeerPresence {
                        key: vec![1],
                        payload: PeerPresencePayload::Selection { start: 0, end: 1 },
                    },
                    colour: None,
                },
                None,
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
                    workspace: WORKSPACE.into(),
                    device_cert: device_cert(),
                },
                None,
            ),
            (
                NeighbourMsg::IdentityAnnouncementV2 {
                    workspace: WORKSPACE.into(),
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
                    workspace: WORKSPACE.into(),
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
                    workspace: WORKSPACE.into(),
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
                    cluster_id: WORKSPACE.into(),
                    hash: "ab".into(),
                },
                Some(read()),
            ),
            (
                NeighbourMsg::RecordContentPiece {
                    responder: 1,
                    requester: 2,
                    app_id: "so.tilo.notes".into(),
                    cluster_id: WORKSPACE.into(),
                    hash: "ab".into(),
                    total: 1,
                    offset: 0,
                    bytes: vec![1],
                },
                Some(read()),
            ),
            (
                NeighbourMsg::RecordContentLacks {
                    responder: 1,
                    requester: 2,
                    app_id: "so.tilo.notes".into(),
                    cluster_id: WORKSPACE.into(),
                    hash: "ab".into(),
                },
                Some(read()),
            ),
            (
                NeighbourMsg::MlsRoutedAccessRequest {
                    room: WORKSPACE.into(),
                    request: Box::new(MlsAccessRequestV1 {
                        version: 1,
                        channel,
                        device: Ed25519PublicKey::from_bytes([2; 32]),
                        key_package: Vec::new(),
                        issued_at: 0,
                        signature: Signature::from_bytes([0xCD; 64]),
                    }),
                    route: Box::new(collab_types::ChannelRoute::new("so.tilo.notes", "notes")),
                    delivery: 0,
                },
                Some(read()),
            ),
            (
                NeighbourMsg::SurfaceKeyframeRequest {
                    peer: 1,
                    surface: 7,
                    seq: 3,
                },
                Some(read()),
            ),
            (
                NeighbourMsg::LivePresence {
                    peer: 1,
                    frame: vec![1],
                },
                Some(read()),
            ),
            (
                NeighbourMsg::SurfaceViewRequest {
                    peer: 1,
                    room: WORKSPACE.into(),
                    views: vec![neighbour_wire::SurfaceView {
                        tab: 1,
                        width: 16,
                        height: 16,
                    }],
                    decode: Vec::new(),
                },
                Some(read()),
            ),
        ]
    }

    #[test]
    fn required_workspace_action_is_explicit_for_every_variant() {
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
        assert_eq!(table.len(), 30, "every NeighbourMsg variant has an entry");
        for (message, expected) in &table {
            assert_eq!(required_workspace_action(message), *expected, "{message:?}");
            // A retired generation is never admitted as a workspace's traffic.
            if message.is_retired() {
                assert_eq!(*expected, None, "{message:?}");
            }
        }
    }
}
