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
    /// The records local app windows hold, by the window instance the
    /// shell launched each for: what each window shows, known as soon as its
    /// app opens a record, before anyone points in it.
    pub fn window_records(&self) -> Vec<(u64, CrdtRoute)> {
        let Ok(instances) = self.window_instances.lock().map(|windows| windows.clone()) else {
            return Vec::new();
        };
        let Ok(channels) = self.channels.lock() else {
            return Vec::new();
        };
        let mut held: Vec<(u64, CrdtRoute)> = channels
            .iter()
            .flat_map(|(key, subscribers)| {
                subscribers.op.iter().filter_map(|sub| {
                    let instance = *instances.get(&sub.peer_id)?;
                    let route = CrdtRoute::from_channel_key(key);
                    (!route.app_id.is_empty()).then_some((instance, route))
                })
            })
            .collect();
        held.sort_by_key(|(instance, route)| (*instance, route.channel_key()));
        held.dedup();
        held
    }

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

    /// A window's records are known by the window instance its app was
    /// launched for, from when it opens them until it disconnects; each
    /// window of one app has its own connection and instance; an app
    /// launched for no window instance names no window.
    #[test]
    fn the_records_an_app_window_holds_are_known_by_its_window_instance() {
        use crate::collab_broker::FanoutState;
        use crate::collab_outbound::PeerPublisher;
        use std::sync::Arc;
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = super::super::CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        let note = CrdtRoute::record("so.tilo.notes", "room-1", "note-1");
        let other = CrdtRoute::record("so.tilo.sheets", "room-1", "budget");
        state.ensure_channel(&note.channel_key());
        state.ensure_channel(&other.channel_key());
        state.window_instances.lock().unwrap().insert(2, 77);
        // The app's second window, opened in its process on a connection
        // of its own.
        let second = CrdtRoute::record("so.tilo.notes", "room-1", "note-2");
        state.ensure_channel(&second.channel_key());
        state.window_instances.lock().unwrap().insert(4, 78);
        let _note = state.subscribe_ops(&note.channel_key(), 2);
        let _other = state.subscribe_ops(&other.channel_key(), 3);
        let _second = state.subscribe_ops(&second.channel_key(), 4);
        assert_eq!(state.window_records(), [(77, note.clone()), (78, second)]);
        state.remove_connection(4);
        assert_eq!(state.window_records(), [(77, note)], "a window closed");
        state.remove_connection(2);
        assert!(state.window_records().is_empty(), "gone with its app");
    }

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
