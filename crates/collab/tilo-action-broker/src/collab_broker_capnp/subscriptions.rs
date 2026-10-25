//! What a local subscriber of a channel is sent besides live traffic.
use super::{
    BrokerChannels, ChannelSubscribers, ContextEnvelope, CrdtRoute, OpAdmission, OpEnvelope, OpKind,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use tokio::sync::mpsc;

/// The tree records of a room's app (a chain's next part, a container's
/// child) that a peer wrote here, by the app route they belong to, and the
/// records each was told about. The shell announces them once, as the write
/// arrives ([`crate::collab_broker::FanoutState`]'s caller); a window of that
/// app that opens afterwards was not there to hear it, and would otherwise
/// never open a part it needs to show the history its room replayed.
#[derive(Default)]
pub(super) struct TreeRecords(Mutex<HashMap<(String, String), Courier>>);

/// How many records one app's route keeps for its later windows: a chat's
/// chain, or a container's children, reach far more, and a window that opens
/// after all of them re-syncs from the room anyway.
const KEPT_PER_ROUTE: usize = 512;

#[derive(Default)]
struct Courier {
    records: VecDeque<String>,
    /// The window instances (or, for a connection the shell launched no
    /// window for, the connection) each record was already handed to.
    told: HashMap<String, Vec<u64>>,
}

impl TreeRecords {
    /// Remember `records` of `route`'s app, for the windows of that app that
    /// subscribe after this.
    pub(super) fn remember(&self, route: &CrdtRoute, records: impl IntoIterator<Item = String>) {
        if !route.is_publishable() || route.app_id.is_empty() {
            return;
        }
        let Ok(mut all) = self.0.lock() else {
            return;
        };
        let courier = all
            .entry((route.app_id.clone(), route.cluster_id.clone()))
            .or_default();
        for record in records {
            if courier.records.iter().any(|known| *known == record) {
                continue;
            }
            if courier.records.len() >= KEPT_PER_ROUTE
                && let Some(evicted) = courier.records.pop_front()
            {
                courier.told.remove(&evicted);
            }
            courier.told.insert(record.clone(), Vec::new());
            courier.records.push_back(record);
        }
    }

    /// The records of `route`'s app that `window` has not been told about
    /// yet, and say it has been: a record reaches each window once.
    fn untold(&self, route: &CrdtRoute, window: u64) -> Vec<String> {
        let Ok(mut all) = self.0.lock() else {
            return Vec::new();
        };
        let Some(courier) = all.get_mut(&(route.app_id.clone(), route.cluster_id.clone())) else {
            return Vec::new();
        };
        courier
            .records
            .iter()
            .filter(|record| {
                let told = courier.told.entry((*record).clone()).or_default();
                if told.contains(&window) {
                    return false;
                }
                told.push(window);
                true
            })
            .cloned()
            .collect()
    }
}

impl super::CapnpBrokerState {
    /// A peer wrote `record_id` in `app_id`'s tree in `cluster_id`: keep it
    /// for the windows of that app that open later.
    pub fn remember_tree_record(&self, app_id: &str, cluster_id: &str, record_id: &str) {
        if ui::record::RecordId::parse(record_id)
            .and_then(|id| id.tree())
            .is_none()
        {
            return;
        }
        self.tree_records.remember(
            &CrdtRoute::record(app_id, cluster_id, record_id),
            [record_id.to_owned()],
        );
    }

    /// Tell the window subscribing to `channel_id`, when it subscribes to
    /// its app's whole route, about the tree records peers wrote for that app
    /// in that room that it was not told about before: what the shell
    /// announced while no window of the app was there
    /// ([`TreeRecords::remember`]).
    pub(super) fn tell_new_window_its_tree_records(
        &self,
        channel_id: &str,
        peer_id: u64,
        tx: &mpsc::UnboundedSender<ContextEnvelope>,
    ) {
        let route = CrdtRoute::from_channel_key(channel_id);
        if route.app_id.is_empty() || !route.record_id.is_empty() {
            return;
        }
        let window = self
            .window_instances
            .lock()
            .ok()
            .and_then(|windows| windows.get(&peer_id).copied())
            .unwrap_or(peer_id);
        let records = self.tree_records.untold(&route, window);
        if !records.is_empty() {
            let _ = tx.send(ContextEnvelope::RecordsArrived(records));
        }
    }
}

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

    /// A window of the app that opens after the room replayed the chat's
    /// part is told the part exists, so it opens it and shows the messages
    /// posted before its person joined. It is told once, and only what its
    /// own app's tree in its own room holds.
    #[test]
    fn a_window_that_opens_after_the_announcement_is_told_the_parts_peers_wrote() {
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
        let chain =
            ui::record::RecordId::derived("messages", &ui::record::RecordId::room_default());
        let (first_part_id, second_part_id) = (chain.part(0), chain.part(1));
        let (first_part, second_part) = (first_part_id.as_str(), second_part_id.as_str());
        state.remember_tree_record("so.tilo.chat", "room-1", first_part);
        state.remember_tree_record("so.tilo.chat", "room-1", second_part);
        // The chat record itself is no tree record, and another room's part
        // is not this window's.
        state.remember_tree_record("so.tilo.chat", "room-1", chain.as_str());
        state.remember_tree_record("so.tilo.chat", "room-2", first_part);
        let chat = CrdtRoute::app("so.tilo.chat", "room-1");
        let mut first = state.subscribe_context(&chat.channel_key(), 7, None);
        let ContextEnvelope::RecordsArrived(told) = first.try_recv().expect("the parts") else {
            panic!("a window hears which records arrived");
        };
        assert_eq!(told, [first_part.to_owned(), second_part.to_owned()]);

        let mut second = state.subscribe_context(&chat.channel_key(), 8, None);
        let ContextEnvelope::RecordsArrived(told) = second.try_recv().expect("the parts") else {
            panic!("the next window is told too");
        };
        assert_eq!(told, [first_part.to_owned(), second_part.to_owned()]);

        let mut again = state.subscribe_context(&chat.channel_key(), 7, None);
        assert!(
            again.try_recv().is_err(),
            "a window that was told is not told again"
        );

        let notes = CrdtRoute::app("so.tilo.notes", "room-1");
        let mut other_app = state.subscribe_context(&notes.channel_key(), 9, None);
        assert!(
            other_app.try_recv().is_err(),
            "another app's window hears nothing of chat's parts"
        );
        let part = CrdtRoute::record("so.tilo.chat", "room-1", first_part);
        let mut record = state.subscribe_context(&part.channel_key(), 10, None);
        assert!(
            record.try_recv().is_err(),
            "a record's stream carries its own ops, not the chain's shape"
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
