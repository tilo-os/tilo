//! What a local subscriber of a channel is sent besides live traffic.
use super::{BrokerChannels, ChannelSubscribers, ContextEnvelope, CrdtRoute, OpEnvelope, OpKind};
use std::sync::Mutex;
use tokio::sync::mpsc;

/// Replay to a new op subscriber of `channel_id` the ops this broker
/// admitted for its record before. The room delivers a record's history
/// once, when this device connects; an app that opens the record later
/// would otherwise build on changes it never received. Its CRDT drops what
/// it already holds.
pub(super) fn admitted_ops(
    channels: &Mutex<BrokerChannels>,
    channel_id: &str,
    tx: &mpsc::UnboundedSender<OpEnvelope>,
) {
    let route = CrdtRoute::from_channel_key(channel_id);
    if route.app_id.is_empty() {
        return;
    }
    let Ok(mut admitted) = channels.lock() else {
        return;
    };
    let artifact =
        collab_types::document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id);
    for op in &admitted.state_for(artifact).op_log {
        if let Ok(op_bytes) = postcard::to_stdvec(op) {
            let _ = tx.send(OpEnvelope {
                op_bytes,
                from_user_id: hex::encode(op.author.as_bytes()),
                kind: OpKind::Signed,
            });
        }
    }
}

impl super::CapnpBrokerState {
    /// Whether a local app subscribes to exactly `route`'s documents.
    pub fn holds_route(&self, route: &CrdtRoute) -> bool {
        self.channels.lock().is_ok_and(|channels| {
            channels
                .get(&route.channel_key())
                .is_some_and(|subscribers| !subscribers.op.is_empty())
        })
    }
}

/// Send one session-local context frame to every context subscriber of
/// `channels` but `except`.
pub(super) fn context_frame<'a>(
    channels: impl Iterator<Item = &'a mut ChannelSubscribers>,
    except: u64,
    env: &ContextEnvelope,
) {
    for subs in channels {
        subs.context.retain(|sub| {
            if sub.peer_id == except {
                return true;
            }
            sub.tx.send(env.clone()).is_ok()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::{
        DocumentMutation, Ed25519PublicKey, SignedOp, document_artifact_id, document_attestation_id,
    };

    /// A message another person posted reached this device before its app
    /// opened the record it went into: the app still receives it when it
    /// subscribes, though the room sent it only once.
    #[test]
    fn a_late_subscriber_receives_what_was_admitted_before() {
        let route = CrdtRoute::record("so.tilo.chat", "room-1", "derived:part.0@chain");
        let artifact = document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id);
        let op = SignedOp::new_unsigned(
            DocumentMutation::Snapshot(b"part".to_vec())
                .encode()
                .unwrap(),
            artifact,
            Ed25519PublicKey::from_bytes([5; 32]),
            document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id),
            collab_types::OpKind::Change,
            std::time::SystemTime::now(),
            1,
        );
        let channels = Mutex::new(BrokerChannels::default());
        channels
            .lock()
            .unwrap()
            .state_for(artifact)
            .record_validated_op(op.clone());
        let (tx, mut rx) = mpsc::unbounded_channel();
        admitted_ops(&channels, &route.channel_key(), &tx);
        let replayed = rx.try_recv().expect("the admitted op");
        assert_eq!(
            postcard::from_bytes::<SignedOp>(&replayed.op_bytes).unwrap(),
            op
        );
        assert!(rx.try_recv().is_err(), "once");

        let (tx, mut rx) = mpsc::unbounded_channel();
        let other = CrdtRoute::record("so.tilo.chat", "room-1", "another");
        admitted_ops(&channels, &other.channel_key(), &tx);
        assert!(
            rx.try_recv().is_err(),
            "another record's subscriber gets none of it"
        );
    }
}
