//! The room gate: which action in the room an inbound [`NeighbourMsg`] is
//! before its variant-specific handler sees it.
//!
//! One exhaustive match decides it, with no default arm. Appending a variant
//! to the wire vocabulary therefore fails to compile here until someone
//! states what the new message demands — a state-changing message can never
//! slip through on read access because its author forgot this table.

use super::ShellNetwork;
use crate::network::protocol::NeighbourMsg;
use tilo_action_policy::{ActionKind, TreePath, roles::COMMENTS_FIELD};

/// What a room message is: the node below the room it acts at and the kind
/// of action it is.
pub(super) type RoomAction = (TreePath, ActionKind);

impl ShellNetwork {
    /// The action `msg` is, which its authenticated sender must be allowed
    /// before variant-specific validation, or `None` when it is never
    /// admitted as room traffic.
    ///
    /// Publishing media, injecting input, granting control, writing
    /// documents and changing room state are active collaboration actions
    /// and never ride on read access. `None` covers retired generations
    /// and the messages [`Self::dispatch_neighbour_msg`] settles before the
    /// room gate (identity, codec adverts, the person's own lane): one of
    /// those reaching the gate means that earlier dispatch was bypassed, and
    /// it is refused rather than admitted on some default.
    pub(super) fn required_room_action(msg: &NeighbourMsg) -> Option<RoomAction> {
        match msg {
            // Room governance, published media, injected input and control
            // consent change what other members see or run.
            NeighbourMsg::SyncResp { .. }
            | NeighbourMsg::SurfaceFrame { .. }
            | NeighbourMsg::SurfaceStreamStart { .. }
            | NeighbourMsg::SurfaceStreamEnd { .. }
            | NeighbourMsg::RemoteInput { .. }
            | NeighbourMsg::PermissionGrant { .. } => Some((TreePath::root(), ActionKind::Undoable)),
            // A document op: the least any op is (a comment). What each
            // envelope changes is decided again against the author's
            // standing (`peer_document_author`) by the broker.
            NeighbourMsg::AppDocument { .. } => Some((
                TreePath::root().field(COMMENTS_FIELD),
                ActionKind::Undoable,
            )),
            // Asking for state the sender may read, or answering with
            // content each piece of which is verified on its own (signed
            // ops, content hashes).
            NeighbourMsg::SyncReq { .. }
            | NeighbourMsg::ScrollbackRequest { .. }
            | NeighbourMsg::ScrollbackResponse { .. }
            | NeighbourMsg::RecordContentWant { .. }
            | NeighbourMsg::RecordContentPiece { .. }
            | NeighbourMsg::RecordContentLacks { .. }
            // A viewer asking for a keyframe of a stream it receives; the
            // owner checks it is one.
            | NeighbourMsg::SurfaceKeyframeRequest { .. }
            // A member saying which of the receiver's windows it views: it
            // may see the room's windows, and is streamed them read only.
            | NeighbourMsg::SurfaceViewRequest { .. }
            // Ephemeral presence and liveness of a member who can see the
            // room; nothing is written to room state.
            | NeighbourMsg::PresenceMoved { .. }
            | NeighbourMsg::PresenceCleared { .. }
            | NeighbourMsg::Heartbeat { .. }
            | NeighbourMsg::AppPresenceV2 { .. }
            | NeighbourMsg::LivePresence { .. }
            // The sender's own device certificate, which only the room
            // governor may promote into governance.
            | NeighbourMsg::IdentityAnnouncementV2 { .. }
            // Each remote action is authorized per call against its own
            // declared permission.
            | NeighbourMsg::RemoteAction { .. }
            // Channel governance is steward-signed and hash-chained; access
            // requests and acks are how a reader obtains and confirms keys.
            | NeighbourMsg::MlsGovernance { .. }
            | NeighbourMsg::MlsAccessRequest { .. }
            | NeighbourMsg::MlsRoutedAccessRequest { .. }
            | NeighbourMsg::MlsGovernanceAck { .. } => Some((TreePath::root(), ActionKind::Read)),
            // Settled before the room gate: identity is how a sender becomes
            // known at all, a codec advert describes the sender's device, and
            // the personal lane is the person's own topic, never a room's.
            NeighbourMsg::IdentityHello { .. }
            | NeighbourMsg::MediaCapabilities { .. }
            | NeighbourMsg::PersonalContinuityRequest { .. }
            | NeighbourMsg::PersonalContinuityCheckpoint { .. }
            | NeighbourMsg::PersonalWorkspaces { .. }
            | NeighbourMsg::RoomArchive { .. }
            // Retired generations, refused where they enter
            // (`NeighbourMsg::is_retired`).
            | NeighbourMsg::IdentityAnnouncement { .. }
            | NeighbourMsg::PermissionRequest { .. } => None,
        }
    }
}
