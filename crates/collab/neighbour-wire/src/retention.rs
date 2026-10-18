//! Which messages a durable host may retain, and on which channel.
//!
//! Only replayable state is allowed into a host's durable queue. Everything
//! else is live session traffic; persisting input, presence, media,
//! permission prompts or request/response control frames would make them
//! execute late.
//!
//! This is the one host-retention classifier: native publication, the relay
//! and browser peers all call it, so a host keeps the same history whichever
//! client wrote it.

use crate::NeighbourMsg;

/// How a retained message relates to the others on its channel.
///
/// The variant order is wire format: delivery handoffs serialize it
/// (postcard variant index `Checkpoint = 0`, `Delta = 1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Retention {
    /// A complete, idempotent state baseline: the host replaces the previous
    /// checkpoint and its older tail on the same channel.
    Checkpoint,
    /// An incremental change appended after the channel's checkpoint.
    Delta,
}

/// The devices a host delivers `msg` to as it arrives, when only they can
/// use it: the copy of a channel admission carrying what its devices join
/// from (every other member gets the copy without it). `None` for a message
/// every member of the room gets. A device catching up replays it like any
/// retained message.
pub fn audience(msg: &NeighbourMsg) -> Option<&[collab_types::Ed25519PublicKey]> {
    let NeighbourMsg::MlsGovernance { frame, .. } = msg else {
        return None;
    };
    match &frame.action {
        collab_types::MlsGovernanceAction::AddWithKeyHistory {
            recipients,
            joining: collab_types::Joining::Material(_),
            ..
        } => Some(recipients),
        _ => None,
    }
}

/// Whether only a channel's steward reads `msg`: a device's access request
/// (plain or routed) and its acknowledgements of the steward's frames. The
/// steward is the device that governs the channel's room; every other
/// member ignores these. A host that governs the room itself, and so
/// stewards its channels, need deliver them to no member and replay them
/// to none.
pub fn steward_only(msg: &NeighbourMsg) -> bool {
    matches!(
        msg,
        NeighbourMsg::MlsAccessRequest { .. }
            | NeighbourMsg::MlsRoutedAccessRequest { .. }
            | NeighbourMsg::MlsGovernanceAck { .. }
    )
}

/// Whether checkpoint `msg` is the whole of its route: its author's next
/// one on the same channel leaves it nothing to add, so a host keeps only
/// the latest. A device's latest certificate, access request and
/// acknowledgement, and a channel's governance head, are each complete on
/// their own; an Automerge checkpoint is not (whether it covers what came
/// before is proven from the document).
pub fn standalone(msg: &NeighbourMsg) -> bool {
    match msg {
        NeighbourMsg::MlsGovernance { frame, .. } => {
            // Each genesis keeps a route of its own.
            !matches!(frame.action, collab_types::MlsGovernanceAction::Genesis)
        }
        NeighbourMsg::MlsAccessRequest { .. }
        | NeighbourMsg::MlsRoutedAccessRequest { .. }
        | NeighbourMsg::MlsGovernanceAck { .. }
        | NeighbourMsg::IdentityHello { .. } => true,
        _ => false,
    }
}

/// Classify `msg` for durable retention.
///
/// Returns the 32-byte channel digest the host keys the message under and
/// whether it is a checkpoint or a delta, or `None` for live traffic that
/// must never be persisted. `shell_checkpoint` marks a shell-tree
/// [`NeighbourMsg::SyncResp`] that carries the full document rather than an
/// incremental tail.
pub fn retention(msg: &NeighbourMsg, shell_checkpoint: bool) -> Option<([u8; 32], Retention)> {
    let (route, kind) = match msg {
        NeighbourMsg::SyncResp { .. } => (
            b"shell-tree".as_slice(),
            if shell_checkpoint {
                Retention::Checkpoint
            } else {
                Retention::Delta
            },
        ),
        NeighbourMsg::AppDocument {
            app_id,
            record_id,
            envelope,
            ..
        } => {
            let channel = app_document_channel(app_id, record_id);
            // Content-key ciphertext carries its storage class in the signed
            // encryption metadata, so a host retains encrypted history
            // without reading it. A single-use MLS application message is
            // never retained: no later reader could open it.
            if let Some(metadata) = envelope.encryption {
                let retention = match metadata.retention()? {
                    collab_types::ContentRetention::Checkpoint => Retention::Checkpoint,
                    collab_types::ContentRetention::Delta => Retention::Delta,
                };
                return Some((channel, retention));
            }
            let frame = collab_types::DocumentFrame::decode(&envelope.payload).ok()?;
            // A segmented snapshot replaces the checkpoint with its first
            // segment and retains the rest as the tail behind it, so a
            // replaying device receives every segment of it in order.
            let retention = match frame {
                collab_types::DocumentFrame::Whole(collab_types::DocumentMutation::Snapshot(_)) => {
                    Retention::Checkpoint
                }
                collab_types::DocumentFrame::Segment(segment)
                    if segment.kind == collab_types::DocumentMutationKind::Snapshot
                        && segment.index == 0 =>
                {
                    Retention::Checkpoint
                }
                collab_types::DocumentFrame::Whole(
                    collab_types::DocumentMutation::Incremental(_),
                )
                | collab_types::DocumentFrame::Segment(_) => Retention::Delta,
            };
            return Some((channel, retention));
        }
        // Channel governance is replayable state a device that was offline
        // must still receive, and none of it is content: the channel's
        // genesis, its newest frame, and the latest access request and
        // acknowledgement of each device. Genesis is kept for good: it names
        // the channel's steward. Every later frame replaces the one before:
        // a joining device needs only the admission that names it, and one
        // that missed a commit asks again. Keeping every commit and Welcome
        // made each newcomer replay the channel's whole history, every
        // Welcome carrying the group's ratchet tree.
        NeighbourMsg::MlsGovernance { frame, .. } => {
            let mut route = b"mls-governance\0".to_vec();
            route.extend_from_slice(frame.channel.0.as_bytes());
            if matches!(frame.action, collab_types::MlsGovernanceAction::Genesis) {
                route.extend_from_slice(&frame.digest().ok()?);
            } else if audience(msg).is_some() {
                // The copy carrying what an admission's devices join from
                // keeps its own route: the head is every member's copy.
                route.extend_from_slice(b"join");
            } else {
                route.extend_from_slice(b"head");
            }
            return Some((*blake3::hash(&route).as_bytes(), Retention::Checkpoint));
        }
        NeighbourMsg::MlsAccessRequest { request, .. } => {
            let mut route = b"mls-access\0".to_vec();
            route.extend_from_slice(request.channel.0.as_bytes());
            route.extend_from_slice(request.device.as_bytes());
            return Some((*blake3::hash(&route).as_bytes(), Retention::Checkpoint));
        }
        NeighbourMsg::MlsRoutedAccessRequest { request, .. } => {
            let mut route = b"mls-routed-access\0".to_vec();
            route.extend_from_slice(request.channel.0.as_bytes());
            route.extend_from_slice(request.device.as_bytes());
            return Some((*blake3::hash(&route).as_bytes(), Retention::Checkpoint));
        }
        NeighbourMsg::MlsGovernanceAck { ack, .. } => {
            let mut route = b"mls-ack\0".to_vec();
            route.extend_from_slice(ack.channel.0.as_bytes());
            route.extend_from_slice(ack.device.as_bytes());
            return Some((*blake3::hash(&route).as_bytes(), Retention::Checkpoint));
        }
        // A device's certificate, sent through a host by a member no gossip
        // neighbour can vouch for (a browser, a shell reachable only through
        // the host). Receivers bind it to the host-authenticated author, and
        // a device that joins later needs it before that member's edits, so
        // the host keeps each device's latest one.
        NeighbourMsg::IdentityHello { cert, .. } => {
            let mut route = b"identity-hello\0".to_vec();
            route.extend_from_slice(&cert.device_public_key);
            return Some((*blake3::hash(&route).as_bytes(), Retention::Checkpoint));
        }
        NeighbourMsg::PersonalContinuityCheckpoint {
            user_public_key, ..
        } => {
            let mut route = Vec::new();
            route.extend_from_slice(b"personal-continuity\0");
            route.extend_from_slice(user_public_key);
            return Some((*blake3::hash(&route).as_bytes(), Retention::Checkpoint));
        }
        // Each device's latest signed directory snapshot is the whole of
        // what it knows, so a device that was offline catches up from the
        // newest one per author.
        NeighbourMsg::PersonalWorkspaces {
            user_public_key,
            update,
        } => {
            let mut route = Vec::new();
            route.extend_from_slice(b"personal-workspaces\0");
            route.extend_from_slice(user_public_key);
            route.extend_from_slice(&update.author_device_public_key);
            return Some((*blake3::hash(&route).as_bytes(), Retention::Checkpoint));
        }
        _ => return None,
    };
    Some((*blake3::hash(route).as_bytes(), kind))
}

/// The retained channel of an app record. Room isolation is supplied by the
/// enclosing authenticated workspace, not by this digest.
#[must_use]
pub fn app_document_channel(app: &str, record: &str) -> [u8; 32] {
    *blake3::hash(format!("app\0{app}\0{record}").as_bytes()).as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(mutation: &collab_types::DocumentMutation) -> NeighbourMsg {
        let envelope = collab_types::SignedOp::new_unsigned(
            mutation.encode().expect("typed mutation"),
            collab_types::ArtifactId::new(),
            collab_types::Ed25519PublicKey::from_bytes([1; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpKind::Change,
            std::time::SystemTime::UNIX_EPOCH,
            1,
        );
        NeighbourMsg::AppDocument {
            peer: 1,
            app_id: "app".into(),
            cluster_id: "room".into(),
            record_id: "doc".into(),
            envelope: Box::new(envelope),
        }
    }

    fn frame(payload: Vec<u8>) -> NeighbourMsg {
        let mut msg = document(&collab_types::DocumentMutation::Incremental(vec![0]));
        if let NeighbourMsg::AppDocument { envelope, .. } = &mut msg {
            envelope.payload = payload;
        }
        msg
    }

    /// Every segment of a large snapshot is kept after its checkpoint, content-key ciphertext is
    /// kept by its signed class, and state persists while late session actions never do.
    /// The channel digests retained state is keyed by stay stable.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn the_host_retains_exactly_what_its_signed_classes_allow() {
        host_retains_every_segment_of_a_large_snapshot_after_its_checkpoint();
        host_retains_content_key_ciphertext_by_its_signed_class();
        host_retains_channel_governance_per_channel_and_device();
        host_persists_state_but_never_late_session_actions();
        channel_digests_are_stable();
        retention_wire_bytes_are_frozen();
    }

    /// The host replays a checkpoint followed by its tail. A snapshot too
    /// large for one frame must therefore start a new checkpoint with its
    /// first segment and keep every later segment in the tail, or a device
    /// replaying from the host could never reassemble it.
    fn host_retains_every_segment_of_a_large_snapshot_after_its_checkpoint() {
        let len = 3 * collab_types::document_transfer::DOCUMENT_SEGMENT_BYTES + 1;
        for (mutation, first) in [
            (
                collab_types::DocumentMutation::Snapshot(vec![5; len]),
                "checkpoint",
            ),
            (
                collab_types::DocumentMutation::Incremental(vec![6; len]),
                "delta",
            ),
        ] {
            let frames = mutation.encode_frames().expect("segmented");
            assert_eq!(frames.len(), 4);
            let retained: Vec<_> = frames
                .into_iter()
                .map(|payload| match retention(&frame(payload), false) {
                    Some((_, Retention::Checkpoint)) => "checkpoint",
                    Some((_, Retention::Delta)) => "delta",
                    None => "dropped",
                })
                .collect();
            assert_eq!(retained, [first, "delta", "delta", "delta"]);
        }
    }

    /// Encrypted history is retained by its signed storage class, never by
    /// decoding ciphertext; single-use MLS messages are not retained.
    fn host_retains_content_key_ciphertext_by_its_signed_class() {
        let channel = collab_types::ChannelId::new();
        let classify = |metadata| {
            let mut msg = frame(vec![0xde, 0xad]);
            if let NeighbourMsg::AppDocument { envelope, .. } = &mut msg {
                envelope.encryption = Some(metadata);
            }
            retention(&msg, false).map(|(_, retention)| retention)
        };
        assert_eq!(
            classify(collab_types::EncryptionMetadata::content_key(
                3,
                channel,
                collab_types::ContentRetention::Checkpoint,
            )),
            Some(Retention::Checkpoint)
        );
        assert_eq!(
            classify(collab_types::EncryptionMetadata::content_key(
                3,
                channel,
                collab_types::ContentRetention::Delta,
            )),
            Some(Retention::Delta)
        );
        assert_eq!(
            classify(collab_types::EncryptionMetadata::mls_application(
                3, channel
            )),
            None
        );
    }

    /// A device that was offline gets a channel's genesis and newest frame,
    /// and the steward gets each device's latest request and
    /// acknowledgement.
    fn host_retains_channel_governance_per_channel_and_device() {
        use collab_types::{
            ChannelId, MlsAccessRequestV1, MlsGovernanceAckV1, MlsGovernanceAction,
            MlsGovernanceFrameV1,
        };
        let steward = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
        let other = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let channel = ChannelId::new();
        let frame = |previous| NeighbourMsg::MlsGovernance {
            room: "room".into(),
            frame: Box::new(
                MlsGovernanceFrameV1::signed(
                    channel,
                    [1; 32],
                    1,
                    previous,
                    0,
                    MlsGovernanceAction::Genesis,
                    &steward,
                )
                .unwrap(),
            ),
            delivery: 0,
        };
        let first = retention(&frame([0; 32]), false).unwrap();
        assert_eq!(first, retention(&frame([0; 32]), false).unwrap());
        assert_ne!(first.0, retention(&frame([1; 32]), false).unwrap().0);
        assert_eq!(first.1, Retention::Checkpoint);
        // Every later frame replaces the channel's previous one.
        let change = |epoch| NeighbourMsg::MlsGovernance {
            room: "room".into(),
            frame: Box::new(
                MlsGovernanceFrameV1::signed(
                    channel,
                    [epoch; 32],
                    u64::from(epoch),
                    [epoch; 32],
                    u64::from(epoch),
                    MlsGovernanceAction::Remove {
                        removed: collab_types::Ed25519PublicKey::from_bytes(
                            device.verifying_key().to_bytes(),
                        ),
                        commit: vec![epoch],
                    },
                    &steward,
                )
                .unwrap(),
            ),
            delivery: 0,
        };
        let head = retention(&change(2), false).unwrap();
        assert_eq!(head, retention(&change(3), false).unwrap());
        assert_ne!(head.0, first.0);

        let request = |key: &ed25519_dalek::SigningKey| NeighbourMsg::MlsAccessRequest {
            room: "room".into(),
            request: Box::new(MlsAccessRequestV1::signed(channel, vec![1], 1, key).unwrap()),
            delivery: 0,
        };
        let (mine, kind) = retention(&request(&device), false).unwrap();
        assert_eq!(kind, Retention::Checkpoint);
        assert_ne!(mine, retention(&request(&other), false).unwrap().0);
        assert_ne!(mine, first.0);
        // The routed form is kept beside the plain one, per device too.
        let routed = |key: &ed25519_dalek::SigningKey| NeighbourMsg::MlsRoutedAccessRequest {
            room: "room".into(),
            request: Box::new(MlsAccessRequestV1::signed(channel, vec![1], 1, key).unwrap()),
            route: Box::new(collab_types::ChannelRoute::new("app", "record")),
            delivery: 0,
        };
        let (routed_mine, kind) = retention(&routed(&device), false).unwrap();
        assert_eq!(kind, Retention::Checkpoint);
        assert!(standalone(&routed(&device)));
        assert_ne!(routed_mine, mine);
        assert_ne!(routed_mine, retention(&routed(&other), false).unwrap().0);

        let ack = NeighbourMsg::MlsGovernanceAck {
            room: "room".into(),
            ack: Box::new(MlsGovernanceAckV1::signed(channel, [4; 32], &device).unwrap()),
            delivery: 0,
        };
        let (acked, kind) = retention(&ack, false).unwrap();
        assert_eq!(kind, Retention::Checkpoint);
        assert_ne!(acked, mine);

        // Requests and acknowledgements are the steward's alone; the frames
        // it sends every member, and the genesis naming it, are not.
        assert!(steward_only(&request(&device)));
        assert!(steward_only(&routed(&device)));
        assert!(steward_only(&ack));
        assert!(!steward_only(&frame([0; 32])));
        assert!(!steward_only(&change(2)));
    }

    fn host_persists_state_but_never_late_session_actions() {
        assert!(matches!(
            retention(
                &NeighbourMsg::SyncResp {
                    changes: vec![vec![1]],
                    room: "room".into(),
                },
                true,
            ),
            Some((_, Retention::Checkpoint))
        ));
        assert!(matches!(
            retention(
                &NeighbourMsg::SyncResp {
                    changes: vec![vec![1]],
                    room: "room".into(),
                },
                false,
            ),
            Some((_, Retention::Delta))
        ));
        assert!(matches!(
            retention(
                &document(&collab_types::DocumentMutation::Snapshot(vec![2])),
                false,
            ),
            Some((_, Retention::Checkpoint))
        ));
        assert!(matches!(
            retention(
                &document(&collab_types::DocumentMutation::Incremental(vec![2])),
                false,
            ),
            Some((_, Retention::Delta))
        ));
        let root_a = [0xA1; 32];
        let root_b = [0xB2; 32];
        let checkpoint_a = NeighbourMsg::PersonalContinuityCheckpoint {
            user_public_key: root_a,
            publisher: 1,
            snapshot: vec![7],
        };
        let checkpoint_b = NeighbourMsg::PersonalContinuityCheckpoint {
            user_public_key: root_b,
            publisher: 2,
            snapshot: vec![8],
        };
        let Some((channel_a, Retention::Checkpoint)) = retention(&checkpoint_a, false) else {
            panic!("personal continuity checkpoints must be retained baselines");
        };
        let Some((channel_b, Retention::Checkpoint)) = retention(&checkpoint_b, false) else {
            panic!("personal continuity checkpoints must be retained baselines");
        };
        assert_ne!(
            channel_a, channel_b,
            "each user root has an isolated host channel"
        );
        assert!(
            retention(
                &NeighbourMsg::PersonalContinuityRequest {
                    user_public_key: root_a,
                    requester: 1,
                },
                false,
            )
            .is_none(),
            "requests are live probes, never retained work"
        );
        for live in [
            NeighbourMsg::RemoteInput {
                peer: 1,
                surface: 2,
                event: vec![3],
            },
            NeighbourMsg::PermissionGrant {
                peer: 1,
                capability: "remote-input".into(),
            },
            NeighbourMsg::RemoteAction {
                frame: b"{}".to_vec(),
            },
            NeighbourMsg::AppPresenceV2 {
                peer: 1,
                app_id: "app".into(),
                cluster_id: "room".into(),
                presence: collab_types::PeerPresence {
                    key: vec![4],
                    payload: collab_types::PeerPresencePayload::Arrived,
                },
                colour: None,
            },
        ] {
            assert!(retention(&live, false).is_none());
        }
    }

    /// Channel digests are what a durable host already keys stored
    /// checkpoints by; changing the route bytes would orphan retained state.
    fn channel_digests_are_stable() {
        let (channel, _) = retention(
            &NeighbourMsg::SyncResp {
                changes: Vec::new(),
                room: String::new(),
            },
            false,
        )
        .expect("shell tree is retained");
        assert_eq!(channel, *blake3::hash(b"shell-tree").as_bytes());
        let (channel, _) = retention(
            &document(&collab_types::DocumentMutation::Snapshot(Vec::new())),
            false,
        )
        .expect("documents are retained");
        assert_eq!(channel, *blake3::hash(b"app\0app\0doc").as_bytes());
        assert_eq!(channel, app_document_channel("app", "doc"));
    }

    /// Delivery handoffs serialize the class beside each governance item;
    /// reordering the variants would silently swap checkpoints and deltas.
    fn retention_wire_bytes_are_frozen() {
        for (class, byte) in [(Retention::Checkpoint, 0u8), (Retention::Delta, 1)] {
            let bytes = postcard::to_allocvec(&class).expect("retention encodes");
            assert_eq!(bytes, [byte]);
            assert_eq!(
                postcard::from_bytes::<Retention>(&bytes).expect("retention decodes"),
                class
            );
        }
    }
}
