//! What a local subscriber of a channel is sent besides live traffic.
use super::{
    BrokerChannels, ChannelSubscribers, ContextEnvelope, CrdtRoute, OpAdmission, OpEnvelope, OpKind,
};
use std::sync::Mutex;
use tokio::sync::mpsc;

/// This broker's verdict for `envelope` on `route`'s channel as it fans out
/// live: a sealed op is sealed because the channel requires it, and
/// cleartext is admitted exactly while the channel's gates admit it. The
/// app applies the verdict and keeps no policy of its own.
pub(super) fn live_admission(
    channels: &BrokerChannels,
    route: &CrdtRoute,
    envelope: &collab_types::SignedOp,
) -> OpAdmission {
    let channel =
        collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record_id);
    channels
        .document_admission(channel, envelope.encryption.as_ref())
        .into()
}

/// Replay to a new op subscriber of `channel_id` the ops this broker
/// admitted for its record before. The room delivers a record's history
/// once, when this device connects; an app that opens the record later
/// would otherwise build on changes it never received. Its CRDT drops what
/// it already holds. Every op in the log passed the channel's gate when it
/// arrived, so cleartext there stays admitted after the channel later
/// starts requiring encryption: the history is the channel's own.
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
                admission: if op.encryption.is_some() {
                    OpAdmission::EncryptionRequired
                } else {
                    OpAdmission::CleartextAdmitted
                },
            });
        }
    }
}

/// Tell a new context subscriber of `channel_id` the records of its app's
/// trees this device holds in its room ([`BrokerChannels::tree_records_held`]).
/// The room replays its records to a device once, when it joins, before any
/// window of an app may run; a window opened later, or after a restart,
/// would otherwise never hold them (a chat's messages from before this
/// device joined). Opening a record still asks its channel's steward for
/// the keys, as for any record.
pub(super) fn held_tree_records(
    channels: &Mutex<BrokerChannels>,
    channel_id: &str,
    tx: &mpsc::UnboundedSender<ContextEnvelope>,
) {
    let route = CrdtRoute::from_channel_key(channel_id);
    let held = channels
        .lock()
        .map(|mut channels| channels.tree_records_held(&route))
        .unwrap_or_default();
    if !held.is_empty() {
        let _ = tx.send(ContextEnvelope::RecordsArrived(held));
    }
}

impl super::CapnpBrokerState {
    /// The verdict a document event fans out with, before the subscribers
    /// are locked; `None` when the gates are unavailable.
    pub(super) fn admission_of(
        &self,
        event: &super::CollabEvent,
        route: &CrdtRoute,
    ) -> Option<OpAdmission> {
        let super::CollabEvent::Document { envelope, .. } = event else {
            return Some(OpAdmission::Unstated);
        };
        let broker_channels = self.broker_channels.lock().ok()?;
        Some(live_admission(&broker_channels, route, envelope))
    }

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

    /// An op of `record` of `app` in `room`, admitted the way the broker
    /// admits every op it validated ([`BrokerChannels::record_history`] and
    /// the channel's op log); sealed under a content key when `sealed`.
    fn admit(channels: &Mutex<BrokerChannels>, app: &str, room: &str, record: &str, sealed: bool) {
        let route = CrdtRoute::record(app, room, record);
        let artifact = document_artifact_id(app, room, record);
        let mut op = SignedOp::new_unsigned(
            DocumentMutation::Incremental(record.as_bytes().to_vec())
                .encode()
                .unwrap(),
            artifact,
            Ed25519PublicKey::from_bytes([5; 32]),
            document_attestation_id(app, room, record),
            collab_types::OpKind::Change,
            std::time::SystemTime::now(),
            1,
        );
        if sealed {
            op.encryption = Some(collab_types::EncryptionMetadata::content_key(
                1,
                collab_types::document_channel_id(app, room, record),
                collab_types::ContentRetention::Delta,
            ));
        }
        let mut channels = channels.lock().unwrap();
        channels.record_history(&route, &op);
        channels.state_for(artifact).record_validated_op(op);
    }

    /// The records each `RecordsArrived` the subscriber was sent named.
    fn told(rx: &mut mpsc::UnboundedReceiver<ContextEnvelope>) -> Vec<Vec<String>> {
        let mut told = Vec::new();
        while let Ok(envelope) = rx.try_recv() {
            if let ContextEnvelope::RecordsArrived(records) = envelope {
                told.push(records);
            }
        }
        told
    }

    /// A broker over `channels`, with the receiver of its action
    /// directory bridge, kept for as long as the broker is used.
    fn broker(
        channels: &std::sync::Arc<Mutex<BrokerChannels>>,
    ) -> (
        std::sync::Arc<super::super::CapnpBrokerState>,
        std::sync::mpsc::Receiver<super::super::ActionDirectoryBridgeRequest>,
    ) {
        use crate::collab_broker::FanoutState;
        use crate::collab_outbound::PeerPublisher;
        use std::sync::Arc;
        super::super::CapnpBrokerState::new(
            &Arc::new(FanoutState::new()),
            Arc::clone(channels),
            PeerPublisher::disabled(),
            None,
            None,
        )
    }

    const CHAT: &str = "so.tilo.chat";

    /// Chat's messages: a chain of parts of the room's default record.
    fn messages() -> ui::record::RecordId {
        ui::record::RecordId::derived("messages", &ui::record::RecordId::room_default())
    }

    /// Part `index` of Chat's messages.
    fn part(index: u64) -> String {
        messages().part(index).to_string()
    }

    /// A child of a note, a record of the note's tree.
    fn note_child() -> String {
        ui::record::RecordId::for_tab(7)
            .child("a")
            .unwrap()
            .to_string()
    }

    /// A device joined a room whose chat already had messages: the room
    /// replayed it the chat's head part before any Chat window ran here.
    /// The Chat window that opens later is told the part exists as it
    /// subscribes, so it holds the part and reads what was posted before
    /// the device joined; a window of another app or room is not told, nor
    /// is a record of no tree.
    #[test]
    fn a_window_that_subscribes_later_learns_the_records_its_room_replayed() {
        let channels = std::sync::Arc::new(Mutex::new(BrokerChannels::default()));
        let (state, _bridge_rx) = broker(&channels);
        let room = uuid::Uuid::from_u128(7).to_string();
        let elsewhere = uuid::Uuid::from_u128(8).to_string();
        let chat = CrdtRoute::app(CHAT, &room);

        // Replayed at the join, while no window of Chat runs here.
        admit(&channels, CHAT, &room, &part(0), false);
        admit(&channels, CHAT, &room, &messages().to_string(), false);
        admit(&channels, "so.tilo.notes", &room, &note_child(), false);
        admit(&channels, CHAT, &elsewhere, &part(3), false);

        let mut window = state.subscribe_context(&chat.channel_key(), 1, None);
        assert_eq!(told(&mut window), [vec![part(0)]]);
        let mut notes = state.subscribe_context(
            &CrdtRoute::app("so.tilo.notes", &room).channel_key(),
            2,
            None,
        );
        assert_eq!(
            told(&mut notes),
            [vec![note_child()]],
            "another app's tree is its own"
        );
        let mut other_room =
            state.subscribe_context(&CrdtRoute::app(CHAT, &elsewhere).channel_key(), 3, None);
        assert_eq!(
            told(&mut other_room),
            [vec![part(3)]],
            "another room's chat is its own"
        );
        let mut record = state.subscribe_context(
            &CrdtRoute::record(CHAT, &room, part(0)).channel_key(),
            4,
            None,
        );
        assert!(
            told(&mut record).is_empty(),
            "a record's subscriber is no app window"
        );

        // A part written while the window runs reaches it live, once it is
        // admitted, and every later window learns both.
        admit(&channels, CHAT, &room, &part(1), false);
        state.fanout_to_route(
            &super::super::CollabEvent::RecordsArrived {
                records: vec![part(1)],
            },
            u64::MAX,
            &chat,
        );
        assert_eq!(told(&mut window), [vec![part(1)]]);
        let mut second = state.subscribe_context(&chat.channel_key(), 5, None);
        assert_eq!(told(&mut second), [vec![part(0), part(1)]]);
    }

    /// The device holds a chat's sealed parts and its shell restarts: the
    /// room does not replay them again, and the broker starts with nothing
    /// in memory. A Chat window opened then is still told the parts, from
    /// the history this device keeps on disk.
    #[test]
    fn a_window_opened_after_a_restart_learns_the_records_kept_before() {
        use crate::collab_history::CollabHistoryStore;
        let dir = tempfile::tempdir().unwrap();
        let history = || Some(CollabHistoryStore::open(dir.path().join("history")).unwrap());
        let room = uuid::Uuid::from_u128(7).to_string();
        let chat = CrdtRoute::app(CHAT, &room);
        {
            let channels = std::sync::Arc::new(Mutex::new(BrokerChannels {
                history: history(),
                ..Default::default()
            }));
            admit(&channels, CHAT, &room, &part(0), true);
            admit(&channels, CHAT, &room, &part(1), true);
            admit(&channels, "so.tilo.notes", &room, &note_child(), true);
        }

        let channels = std::sync::Arc::new(Mutex::new(BrokerChannels {
            history: history(),
            ..Default::default()
        }));
        let (state, _bridge_rx) = broker(&channels);
        let mut window = state.subscribe_context(&chat.channel_key(), 1, None);
        assert_eq!(told(&mut window), [vec![part(0), part(1)]]);
        let elsewhere = uuid::Uuid::from_u128(8).to_string();
        let mut other_room =
            state.subscribe_context(&CrdtRoute::app(CHAT, &elsewhere).channel_key(), 2, None);
        assert!(told(&mut other_room).is_empty(), "another room's chat");
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

    #[test]
    fn cleartext_admitted_before_the_channel_required_encryption_stays_admitted() {
        let route = CrdtRoute::record("so.tilo.chat", "room-1", "");
        let artifact = document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id);
        let op = SignedOp::new_unsigned(
            DocumentMutation::Incremental(b"general".to_vec())
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
        let admission = || live_admission(&channels.lock().unwrap(), &route, &op);
        assert_eq!(admission(), OpAdmission::CleartextAdmitted);
        channels
            .lock()
            .unwrap()
            .state_for(artifact)
            .record_validated_op(op.clone());
        channels.lock().unwrap().register_channel_gate(
            collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record_id),
            channel_crypto::ChannelGate {
                encryption_required: true,
                head_epoch: None,
            },
        );
        assert_eq!(admission(), OpAdmission::EncryptionRequired);
        let (tx, mut rx) = mpsc::unbounded_channel();
        admitted_ops(&channels, &route.channel_key(), &tx);
        assert_eq!(
            rx.try_recv().unwrap().admission,
            OpAdmission::CleartextAdmitted
        );
    }
}
