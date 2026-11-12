//! What a local subscriber of a channel is sent besides live traffic.
use super::{
    BrokerChannels, ChannelSubscribers, ContextUpdate, CrdtRoute, OpAdmission, OpEnvelope,
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
    channels.document_admission(&route.cluster_id, channel, envelope.encryption.as_ref())
}

/// Replay to a new op subscriber of `channel_id` the ops this broker
/// admitted for its record before. The workspace delivers a record's history
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
/// trees this device holds in its workspace ([`BrokerChannels::tree_records_held`]).
/// The workspace replays its records to a device once, when it joins, before any
/// window of an app may run; a window opened later, or after a restart,
/// would otherwise never hold them (a chat's messages from before this
/// device joined). Opening a record still asks its channel's participants
/// for the keys, as for any record.
pub(super) fn held_tree_records(
    channels: &Mutex<BrokerChannels>,
    channel_id: &str,
    tx: &mpsc::UnboundedSender<ContextUpdate>,
) {
    let route = CrdtRoute::from_channel_key(channel_id);
    let held = channels
        .lock()
        .map(|mut channels| channels.tree_records_held(&route))
        .unwrap_or_default();
    if !held.is_empty() {
        let _ = tx.send(ContextUpdate::RecordsArrived(held));
    }
}

impl super::AppBrokerState {
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

    /// The records local app windows hold, by the window instance each shows:
    /// what each window shows, known as soon as its app opens a record, before
    /// anyone points in it.
    ///
    /// A window names itself: its app subscribes the window's own route
    /// (`continuation:<tab>`, where the app speaks to the session about that
    /// window), so the connection carrying it is that window's, however the
    /// app came to be connected (a window the session connects for it, or the
    /// launcher handing the app its own). The instance the shell recorded for
    /// a connection it made itself answers for a window that names none.
    pub fn window_records(&self) -> Vec<(u64, CrdtRoute)> {
        let Ok(instances) = self.window_instances.lock().map(|windows| windows.clone()) else {
            return Vec::new();
        };
        let Ok(channels) = self.channels.lock() else {
            return Vec::new();
        };
        let mut named = instances;
        for (key, subscribers) in channels.iter() {
            let route = CrdtRoute::from_channel_key(key);
            let Some(tab) = continuation_tab(&route) else {
                continue;
            };
            for sub in &subscribers.op {
                named.insert(sub.peer_id, tab);
            }
        }
        let mut held: Vec<(u64, CrdtRoute)> = channels
            .iter()
            .flat_map(|(key, subscribers)| {
                subscribers.op.iter().filter_map(|sub| {
                    let instance = *named.get(&sub.peer_id)?;
                    let route = CrdtRoute::from_channel_key(key);
                    (!route.app_id.is_empty() && continuation_tab(&route).is_none())
                        .then_some((instance, route))
                })
            })
            .collect();
        held.sort_by_key(|(instance, route)| (*instance, route.channel_key()));
        held.dedup();
        held
    }

    /// The connection of the local app window `instance`, while it is
    /// connected: the ones it named as its own, and the one the shell recorded
    /// for a connection it made. The lowest when a window holds more than one.
    pub fn window_peer(&self, instance: u64) -> Option<u64> {
        let windows = self.window_instances.lock().ok()?.clone();
        // A window that named itself on its own route is its connection, as
        // [`Self::window_records`] reads it.
        let named: Vec<u64> = self
            .channels
            .lock()
            .ok()?
            .iter()
            .filter(|(key, _)| {
                continuation_tab(&CrdtRoute::from_channel_key(key)) == Some(instance)
            })
            .flat_map(|(_, subscribers)| subscribers.op.iter().map(|sub| sub.peer_id))
            .collect();
        windows
            .iter()
            .filter(|(_, at)| **at == instance)
            .map(|(peer, _)| *peer)
            .chain(named)
            .min()
    }

    /// Stand in for the launcher's handoff naming `peer_id` the connection
    /// of the window the shell launched as `instance`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn bind_window_instance_for_test(&self, peer_id: u64, instance: u64) {
        if let Ok(mut windows) = self.window_instances.lock() {
            windows.insert(peer_id, instance);
        }
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
    env: &ContextUpdate,
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

impl super::InvocationEnvelope {}

/// The tab a window's own route names (`continuation:<tab>`): the window a
/// connection carrying it is, as its app says.
fn continuation_tab(route: &CrdtRoute) -> Option<u64> {
    let tab = route.record_id.strip_prefix("continuation:")?;
    // ignored-failure: a route that says `continuation:` and then something that is not a tab number names no window this session holds
    tab.parse().ok()
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
        let (state, _bridge_rx) = super::super::AppBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        let note = CrdtRoute::record("so.tilo.notes", "workspace-1", "note-1");
        let other = CrdtRoute::record("so.tilo.sheets", "workspace-1", "budget");
        state.ensure_channel(&note.channel_key());
        state.ensure_channel(&other.channel_key());
        state.window_instances.lock().unwrap().insert(2, 77);
        let second = CrdtRoute::record("so.tilo.notes", "workspace-1", "note-2");
        state.ensure_channel(&second.channel_key());
        state.window_instances.lock().unwrap().insert(4, 78);
        let _note = state.subscribe_ops(&note.channel_key(), 2);
        let _other = state.subscribe_ops(&other.channel_key(), 3);
        let _second = state.subscribe_ops(&second.channel_key(), 4);
        assert_eq!(state.window_records(), [(77, note.clone()), (78, second)]);
        assert_eq!(
            state.window_peer(78),
            Some(4),
            "where an action for it goes"
        );
        assert_eq!(state.window_peer(79), None, "no window of that instance");
        state.remove_connection(4);
        assert_eq!(state.window_peer(78), None, "not once it closed");
        assert_eq!(state.window_records(), [(77, note)], "a window closed");
        state.remove_connection(2);
        assert!(state.window_records().is_empty(), "gone with its app");
    }

    /// A window whose app was handed its own connection names itself: the
    /// route it speaks to the session about the window on carries its tab, so
    /// its records are still known by the window that shows them — and that
    /// route is never taken for what it shows.
    #[test]
    fn a_window_that_names_itself_is_known_by_the_tab_it_says() {
        use crate::collab_broker::FanoutState;
        use crate::collab_outbound::PeerPublisher;
        use std::sync::Arc;
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = super::super::AppBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        let note = CrdtRoute::record("so.tilo.notes", "workspace-1", "note-1");
        let window = CrdtRoute::record("so.tilo.notes", "workspace-1", "continuation:91");
        state.ensure_channel(&note.channel_key());
        state.ensure_channel(&window.channel_key());
        let _note = state.subscribe_ops(&note.channel_key(), 5);
        let _window = state.subscribe_ops(&window.channel_key(), 5);
        assert_eq!(
            state.window_records(),
            [(91, note)],
            "the record its window shows, by the tab it named"
        );
        assert_eq!(
            state.window_peer(91),
            Some(5),
            "where an action for it goes"
        );
    }

    /// An op of `record` of `app` in `workspace`, admitted the way the broker
    /// admits every op it validated ([`BrokerChannels::record_history`] and
    /// the channel's op log); sealed under a content key when `sealed`.
    fn admit(
        channels: &Mutex<BrokerChannels>,
        app: &str,
        workspace: &str,
        record: &str,
        sealed: bool,
    ) {
        let route = CrdtRoute::record(app, workspace, record);
        let artifact = document_artifact_id(app, workspace, record);
        let mut op = SignedOp::new_unsigned(
            DocumentMutation::Incremental(record.as_bytes().to_vec())
                .encode()
                .unwrap(),
            artifact,
            Ed25519PublicKey::from_bytes([5; 32]),
            document_attestation_id(app, workspace, record),
            collab_types::OpKind::Change,
            tilo_clock::wall(),
            1,
        );
        if sealed {
            op.encryption = Some(collab_types::EncryptionMetadata::content_key(
                1,
                collab_types::document_channel_id(app, workspace, record),
                collab_types::ContentRetention::Delta,
            ));
        }
        let mut channels = channels.lock().unwrap();
        let unsynced = channels.record_history(&route, &op);
        channels.state_for(artifact).record_validated_op(op);
        drop(channels);
        unsynced.sync().unwrap();
    }

    /// The records each `RecordsArrived` the subscriber was sent named.
    fn told(rx: &mut mpsc::UnboundedReceiver<ContextUpdate>) -> Vec<Vec<String>> {
        let mut told = Vec::new();
        while let Ok(envelope) = rx.try_recv() {
            if let ContextUpdate::RecordsArrived(records) = envelope {
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
        std::sync::Arc<super::super::AppBrokerState>,
        std::sync::mpsc::Receiver<super::super::ActionDirectoryBridgeRequest>,
    ) {
        use crate::collab_broker::FanoutState;
        use crate::collab_outbound::PeerPublisher;
        use std::sync::Arc;
        super::super::AppBrokerState::new(
            &Arc::new(FanoutState::new()),
            Arc::clone(channels),
            PeerPublisher::disabled(),
            None,
            None,
        )
    }

    const CHAT: &str = "so.tilo.chat";

    /// Chat's messages: a chain of parts of the workspace's default record.
    fn messages() -> ui::record::RecordId {
        ui::record::RecordId::derived("messages", &ui::record::RecordId::workspace_default())
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

    /// A device joined a workspace whose chat already had messages: the workspace
    /// replayed it the chat's head part before any Chat window ran here.
    /// The Chat window that opens later is told the part exists as it
    /// subscribes, so it holds the part and reads what was posted before
    /// the device joined; a window of another app or workspace is not told, nor
    /// is a record of no tree.
    #[test]
    fn a_window_that_subscribes_later_learns_the_records_its_workspace_replayed() {
        let channels = std::sync::Arc::new(Mutex::new(BrokerChannels::default()));
        let (state, _bridge_rx) = broker(&channels);
        let workspace = uuid::Uuid::from_u128(7).to_string();
        let elsewhere = uuid::Uuid::from_u128(8).to_string();
        let chat = CrdtRoute::app(CHAT, &workspace);

        // Replayed at the join, while no window of Chat runs here.
        admit(&channels, CHAT, &workspace, &part(0), false);
        admit(&channels, CHAT, &workspace, &messages().to_string(), false);
        admit(&channels, "so.tilo.notes", &workspace, &note_child(), false);
        admit(&channels, CHAT, &elsewhere, &part(3), false);

        let mut window = state.subscribe_context(&chat.channel_key(), 1, None);
        assert_eq!(told(&mut window), [vec![part(0)]]);
        let mut notes = state.subscribe_context(
            &CrdtRoute::app("so.tilo.notes", &workspace).channel_key(),
            2,
            None,
        );
        assert_eq!(
            told(&mut notes),
            [vec![note_child()]],
            "another app's tree is its own"
        );
        let mut other_workspace =
            state.subscribe_context(&CrdtRoute::app(CHAT, &elsewhere).channel_key(), 3, None);
        assert_eq!(
            told(&mut other_workspace),
            [vec![part(3)]],
            "another workspace's chat is its own"
        );
        let mut record = state.subscribe_context(
            &CrdtRoute::record(CHAT, &workspace, part(0)).channel_key(),
            4,
            None,
        );
        assert!(
            told(&mut record).is_empty(),
            "a record's subscriber is no app window"
        );

        // A part written while the window runs reaches it live, once it is
        // admitted, and every later window learns both.
        admit(&channels, CHAT, &workspace, &part(1), false);
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
    /// workspace does not replay them again, and the broker starts with nothing
    /// in memory. A Chat window opened then is still told the parts, from
    /// the history this device keeps on disk.
    #[test]
    fn a_window_opened_after_a_restart_learns_the_records_kept_before() {
        use crate::collab_history::CollabHistoryStore;
        let dir = tempfile::tempdir().unwrap();
        let history = || Some(CollabHistoryStore::open(dir.path().join("history")).unwrap());
        let workspace = uuid::Uuid::from_u128(7).to_string();
        let chat = CrdtRoute::app(CHAT, &workspace);
        {
            let channels = std::sync::Arc::new(Mutex::new(BrokerChannels {
                history: history(),
                ..Default::default()
            }));
            admit(&channels, CHAT, &workspace, &part(0), true);
            admit(&channels, CHAT, &workspace, &part(1), true);
            admit(&channels, "so.tilo.notes", &workspace, &note_child(), true);
        }

        let channels = std::sync::Arc::new(Mutex::new(BrokerChannels {
            history: history(),
            ..Default::default()
        }));
        let (state, _bridge_rx) = broker(&channels);
        let mut window = state.subscribe_context(&chat.channel_key(), 1, None);
        assert_eq!(told(&mut window), [vec![part(0), part(1)]]);
        let elsewhere = uuid::Uuid::from_u128(8).to_string();
        let mut other_workspace =
            state.subscribe_context(&CrdtRoute::app(CHAT, &elsewhere).channel_key(), 2, None);
        assert!(
            told(&mut other_workspace).is_empty(),
            "another workspace's chat"
        );
    }

    /// A message another person posted reached this device before its app
    /// opened the record it went into: the app still receives it when it
    /// subscribes, though the workspace sent it only once.
    #[test]
    fn a_late_subscriber_receives_what_was_admitted_before() {
        let route = CrdtRoute::record("so.tilo.chat", "workspace-1", "derived:part.0@chain");
        let artifact = document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id);
        let op = SignedOp::new_unsigned(
            DocumentMutation::Snapshot(b"part".to_vec())
                .encode()
                .unwrap(),
            artifact,
            Ed25519PublicKey::from_bytes([5; 32]),
            document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id),
            collab_types::OpKind::Change,
            tilo_clock::wall(),
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
        let other = CrdtRoute::record("so.tilo.chat", "workspace-1", "another");
        admitted_ops(&channels, &other.channel_key(), &tx);
        assert!(
            rx.try_recv().is_err(),
            "another record's subscriber gets none of it"
        );
    }

    #[test]
    fn cleartext_admitted_before_the_channel_required_encryption_stays_admitted() {
        let route = CrdtRoute::record("so.tilo.chat", "workspace-1", "");
        let artifact = document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id);
        let op = SignedOp::new_unsigned(
            DocumentMutation::Incremental(b"general".to_vec())
                .encode()
                .unwrap(),
            artifact,
            Ed25519PublicKey::from_bytes([5; 32]),
            document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id),
            collab_types::OpKind::Change,
            tilo_clock::wall(),
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
