//! Which messages a durable host may retain, and on which channel.
//!
//! Only replayable state is allowed into a host's durable queue. Everything
//! else is live session traffic; persisting input, presence, media,
//! permission prompts or request/response control frames would make them
//! execute late.

use crate::NeighbourMsg;

/// How a retained message relates to the others on its channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retention {
    /// A complete, idempotent state baseline: the host replaces the previous
    /// checkpoint and its older tail on the same channel.
    Checkpoint,
    /// An incremental change appended after the channel's checkpoint.
    Delta,
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
            let route = format!("app\0{app_id}\0{record_id}");
            let channel = *blake3::hash(route.as_bytes()).as_bytes();
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
        // must still receive: every frame of the chain, and the latest access
        // request and acknowledgement of each device. None of it is content.
        // Each frame is its own baseline, keyed by its digest, so a steward
        // redelivering a frame replaces the retained copy instead of growing
        // the host's tail; receivers restore chain order themselves.
        NeighbourMsg::MlsGovernance { frame, .. } => {
            let digest = frame.digest().ok()?;
            let mut route = b"mls-governance\0".to_vec();
            route.extend_from_slice(frame.channel.0.as_bytes());
            route.extend_from_slice(&digest);
            return Some((*blake3::hash(&route).as_bytes(), Retention::Checkpoint));
        }
        NeighbourMsg::MlsAccessRequest { request, .. } => {
            let mut route = b"mls-access\0".to_vec();
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
        _ => return None,
    };
    Some((*blake3::hash(route).as_bytes(), kind))
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
            collab_types::OpClass::Splice,
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

    /// A device that was offline gets every frame of a channel's chain
    /// (a redelivered frame replaces its copy), and the steward gets each
    /// device's latest request and acknowledgement.
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

        let request = |key: &ed25519_dalek::SigningKey| NeighbourMsg::MlsAccessRequest {
            room: "room".into(),
            request: Box::new(MlsAccessRequestV1::signed(channel, vec![1], 1, key).unwrap()),
            delivery: 0,
        };
        let (mine, kind) = retention(&request(&device), false).unwrap();
        assert_eq!(kind, Retention::Checkpoint);
        assert_ne!(mine, retention(&request(&other), false).unwrap().0);
        assert_ne!(mine, first.0);

        let ack = NeighbourMsg::MlsGovernanceAck {
            room: "room".into(),
            ack: Box::new(MlsGovernanceAckV1::signed(channel, [4; 32], &device).unwrap()),
            delivery: 0,
        };
        let (acked, kind) = retention(&ack, false).unwrap();
        assert_eq!(kind, Retention::Checkpoint);
        assert_ne!(acked, mine);
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
    }
}
