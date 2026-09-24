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
            let mutation = collab_types::DocumentMutation::decode(&envelope.payload).ok()?;
            let retention = match mutation {
                collab_types::DocumentMutation::Snapshot(_) => Retention::Checkpoint,
                collab_types::DocumentMutation::Incremental(_) => Retention::Delta,
            };
            return Some((*blake3::hash(route.as_bytes()).as_bytes(), retention));
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

    #[test]
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
    #[test]
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
