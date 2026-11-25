//! What a local subscriber of a channel is sent besides live traffic.
use super::{BrokerChannels, ChannelSubscribers, ContextUpdate, CrdtRoute};
use std::sync::Mutex;
use tokio::sync::mpsc;

/// Tell a new context subscriber of `channel_id` the records of its app's
/// trees this device holds in its workspace ([`BrokerChannels::tree_records_held`]).
/// The workspace replays its records to a device once, when it joins, before any
/// window of an app may run; a window opened later, or after a restart,
/// would otherwise never hold them (a chat's messages from before this
/// device joined). Opening a record still asks its channel's members
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
        let _ = tx.send(ContextUpdate::ResourcesArrived(held));
    }
}

impl super::AppBrokerState {
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
    pub fn window_resources(&self) -> Vec<(u64, CrdtRoute)> {
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
            for sub in &subscribers.context {
                named.insert(sub.peer_id, tab);
            }
        }
        let mut held: Vec<(u64, CrdtRoute)> = channels
            .iter()
            .flat_map(|(key, subscribers)| {
                subscribers.context.iter().filter_map(|sub| {
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
        // [`Self::window_resources`] reads it.
        let named: Vec<u64> = self
            .channels
            .lock()
            .ok()?
            .iter()
            .filter(|(key, _)| {
                continuation_tab(&CrdtRoute::from_channel_key(key)) == Some(instance)
            })
            .flat_map(|(_, subscribers)| subscribers.context.iter().map(|sub| sub.peer_id))
            .collect();
        windows
            .iter()
            .filter(|(_, at)| **at == instance)
            .map(|(peer, _)| *peer)
            .chain(named)
            .min()
    }

    /// The local app window whose connection is `peer_id`, while it is connected: the one the
    /// shell recorded for a connection it made, or a window that named itself on its own route.
    pub(super) fn window_of_peer(&self, peer_id: u64) -> Option<u64> {
        let instance = self.window_instances.lock().ok()?.get(&peer_id).copied();
        if instance.is_some() {
            return instance;
        }
        self.channels
            .lock()
            .ok()?
            .iter()
            .find_map(|(key, subscribers)| {
                subscribers
                    .context
                    .iter()
                    .any(|sub| sub.peer_id == peer_id)
                    .then(|| continuation_tab(&CrdtRoute::from_channel_key(key)))
                    .flatten()
            })
    }

    /// Stand in for the launcher's handoff naming `peer_id` the connection
    /// of the window the shell launched as `instance`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn bind_window_instance_for_test(&self, peer_id: u64, instance: u64) {
        if let Ok(mut windows) = self.window_instances.lock() {
            windows.insert(peer_id, instance);
        }
        self.windows_changed();
    }

    /// Whether a local app window has exactly `route`'s record open.
    pub fn holds_route(&self, route: &CrdtRoute) -> bool {
        self.channels.lock().is_ok_and(|channels| {
            channels
                .get(&route.channel_key())
                .is_some_and(|subscribers| !subscribers.context.is_empty())
        })
    }
}

/// Send one session-local context frame to every context subscriber of
/// `channels` but `except`. Answers whether a subscriber that went away was
/// let go, so what the windows here hold was re-read by its caller.
pub(super) fn context_frame<'a>(
    channels: impl Iterator<Item = &'a mut ChannelSubscribers>,
    except: u64,
    env: &ContextUpdate,
) -> bool {
    let mut pruned = false;
    for subs in channels {
        let before = subs.context.len();
        subs.context.retain(|sub| {
            if sub.peer_id == except {
                return true;
            }
            sub.tx.send(env.clone()).is_ok()
        });
        pruned |= subs.context.len() != before;
    }
    pruned
}

impl super::InvocationEnvelope {}

/// The tab a window's own route names (`continuation:<tab>`): the window a
/// connection carrying it is, as its app says.
fn continuation_tab(route: &CrdtRoute) -> Option<u64> {
    let tab = route.document_id.strip_prefix("continuation:")?;
    tilo_failure::ignored!(
        "a route that says `continuation:` names no window unless a tab number follows",
        tab.parse().ok()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::{
        DocumentMutation, Ed25519PublicKey, SignedOp, document_artifact_id, document_attestation_id,
    };

    tilo_test_env::scenario! {
        fn a_windows_records_are_known_by_its_instance;

        phase the_records_an_app_window_holds_are_known_by_its_window_instance;
        // A window's records are known by the window instance its app was
        // launched for, from when it opens them until it disconnects; each
        // window of one app has its own connection and instance; an app
        // launched for no window instance names no window.
        {
            use crate::apps::collab_broker::FanoutState;
            use crate::apps::collab_outbound::PeerPublisher;
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
            let _note = state.subscribe_context(&note.channel_key(), 2, None);
            let _other = state.subscribe_context(&other.channel_key(), 3, None);
            let _second = state.subscribe_context(&second.channel_key(), 4, None);
            assert_eq!(state.window_resources(), [(77, note.clone()), (78, second)]);
            assert_eq!(
                state.window_peer(78),
                Some(4),
                "where an action for it goes"
            );
            assert_eq!(state.window_peer(79), None, "no window of that instance");
            state.remove_connection(4);
            assert_eq!(state.window_peer(78), None, "not once it closed");
            assert_eq!(state.window_resources(), [(77, note)], "a window closed");
            state.remove_connection(2);
            assert!(state.window_resources().is_empty(), "gone with its app");
        }

        phase a_window_that_names_itself_is_known_by_the_tab_it_says;
        // A window whose app was handed its own connection names itself: the
        // route it speaks to the session about the window on carries its tab, so
        // its records are still known by the window that shows them — and that
        // route is never taken for what it shows.
        {
            use crate::apps::collab_broker::FanoutState;
            use crate::apps::collab_outbound::PeerPublisher;
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
            let _note = state.subscribe_context(&note.channel_key(), 5, None);
            let _window = state.subscribe_context(&window.channel_key(), 5, None);
            assert_eq!(
                state.window_resources(),
                [(91, note)],
                "the record its window shows, by the tab it named"
            );
            assert_eq!(
                state.window_peer(91),
                Some(5),
                "where an action for it goes"
            );
        }
    }

    /// An op of `record` of `app` in `workspace`, admitted the way the broker
    /// admits every op it validated ([`BrokerChannels::document_history`] and
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
                collab_types::ContentRetention::Delta,
            ));
        }
        let mut channels = channels.lock().unwrap();
        let unsynced = channels.document_history(&route, &op);
        channels.state_for(artifact).record_validated_op(op);
        drop(channels);
        unsynced.sync().unwrap();
    }

    /// The records each `ResourcesArrived` the subscriber was sent named.
    fn told(rx: &mut mpsc::UnboundedReceiver<ContextUpdate>) -> Vec<Vec<String>> {
        let mut told = Vec::new();
        while let Ok(envelope) = rx.try_recv() {
            if let ContextUpdate::ResourcesArrived(records) = envelope {
                told.push(records);
            }
        }
        told
    }

    /// The records each `RecordsMoved` the subscriber was sent named.
    fn moved(rx: &mut mpsc::UnboundedReceiver<ContextUpdate>) -> Vec<Vec<String>> {
        let mut moved = Vec::new();
        while let Ok(envelope) = rx.try_recv() {
            if let ContextUpdate::RecordsMoved(records) = envelope {
                moved.push(records);
            }
        }
        moved
    }

    /// A broker over `channels`, with the receiver of its action
    /// directory bridge, kept for as long as the broker is used.
    fn broker(
        channels: &std::sync::Arc<Mutex<BrokerChannels>>,
    ) -> (
        std::sync::Arc<super::super::AppBrokerState>,
        std::sync::mpsc::Receiver<super::super::ActionDirectoryBridgeRequest>,
    ) {
        use crate::apps::collab_broker::FanoutState;
        use crate::apps::collab_outbound::PeerPublisher;
        use std::sync::Arc;
        super::super::AppBrokerState::new(
            &Arc::new(FanoutState::new()),
            Arc::clone(channels),
            PeerPublisher::disabled(),
            None,
            None,
        )
    }

    const CHAT: &str = tilo_app_id::apps::CHAT;

    /// Chat's messages: a chain of parts of the workspace's default record.
    fn messages() -> collab_types::document::DocumentId {
        collab_types::document::DocumentId::derived(
            "messages",
            &collab_types::document::DocumentId::workspace_default(),
        )
    }

    /// Part `index` of Chat's messages.
    fn part(index: u64) -> String {
        messages().part(index).to_string()
    }

    /// A child of a note, a record of the note's tree.
    fn note_child() -> String {
        collab_types::document::DocumentId::for_tab(7)
            .child("a")
            .unwrap()
            .to_string()
    }

    tilo_test_env::scenario! {
        fn a_late_subscriber_learns_what_was_admitted_before;

        phase a_window_that_subscribes_later_learns_the_records_its_workspace_replayed;
        // A device joined a workspace whose chat already had messages: the workspace
        // replayed it the chat's head part before any Chat window ran here.
        // The Chat window that opens later is told the part exists as it
        // subscribes, so it holds the part and reads what was posted before
        // the device joined; a window of another app or workspace is not told, nor
        // is a record of no tree.
        {
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
                &super::super::CollabEvent::ResourcesArrived {
                    records: vec![part(1)],
                },
                u64::MAX,
                &chat,
            );
            assert_eq!(told(&mut window), [vec![part(1)]]);
            let mut second = state.subscribe_context(&chat.channel_key(), 5, None);
            assert_eq!(told(&mut second), [vec![part(0), part(1)]]);
        }

        phase a_window_opened_after_a_restart_learns_the_records_kept_before;
        // The device holds a chat's sealed parts and its shell restarts: the
        // workspace does not replay them again, and the broker starts with nothing
        // in memory. A Chat window opened then is still told the parts, from
        // the history this device keeps on disk.
        {
            use crate::apps::collab_history::CollabHistoryStore;
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

    }

    /// The route, and the records, of each `ResourcesArrived` a window that holds the documents
    /// was told, in order.
    type Announced = std::sync::Arc<Mutex<Vec<(CrdtRoute, Vec<String>)>>>;

    /// The windows one peer's writes reach, and the broker that carries them: the app's window
    /// opened on `workspace`, another workspace's window, the subscriber of `record` alone, and
    /// what a window that holds the documents is told of the parts a peer wrote.
    struct PeerWrites {
        channels: std::sync::Arc<Mutex<BrokerChannels>>,
        fanout: std::sync::Arc<crate::apps::collab_broker::FanoutState>,
        workspace: String,
        app: CrdtRoute,
        _broker: std::sync::Arc<super::super::AppBrokerState>,
        _bridge: std::sync::mpsc::Receiver<super::super::ActionDirectoryBridgeRequest>,
        window: mpsc::UnboundedReceiver<ContextUpdate>,
        elsewhere: mpsc::UnboundedReceiver<ContextUpdate>,
        record_route: mpsc::UnboundedReceiver<ContextUpdate>,
        announced: Announced,
    }

    /// The windows of `workspace` and of another workspace, a subscriber of `record` alone, and a
    /// window that holds the documents, all open while a peer's writes are delivered.
    fn peer_writes(workspace: &str, record: &str) -> PeerWrites {
        use crate::apps::collab_broker::FanoutState;
        use crate::apps::collab_outbound::PeerPublisher;
        use std::sync::Arc;
        let channels = Arc::new(Mutex::new(BrokerChannels::default()));
        let fanout = Arc::new(FanoutState::new());
        let (broker, bridge) = super::super::AppBrokerState::new(
            &fanout,
            Arc::clone(&channels),
            PeerPublisher::disabled(),
            None,
            None,
        );
        fanout.attach_app_broker(Arc::clone(&broker));
        let app = CrdtRoute::app(CHAT, workspace);
        broker.ensure_channel(&app.channel_key());
        let window = broker.subscribe_context(&app.channel_key(), 1, None);
        let elsewhere = broker.subscribe_context(
            &CrdtRoute::app(CHAT, uuid::Uuid::from_u128(8).to_string()).channel_key(),
            2,
            None,
        );
        let record_route = broker.subscribe_context(
            &CrdtRoute::record(CHAT, workspace, record).channel_key(),
            3,
            None,
        );
        let announced = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&announced);
        let _app = fanout.subscribe_workspaces(
            vec![workspace.to_owned()],
            Box::new(move |event, origin| {
                if let super::super::CollabEvent::ResourcesArrived { records } = event {
                    sink.lock().unwrap().push((origin.clone(), records.clone()));
                }
            }),
        );
        PeerWrites {
            channels,
            fanout,
            workspace: workspace.to_owned(),
            app,
            _broker: broker,
            _bridge: bridge,
            window,
            elsewhere,
            record_route,
            announced,
        }
    }

    impl PeerWrites {
        /// `envelope`, which a peer wrote into `record`, delivered to this broker.
        fn deliver(
            &self,
            record: &str,
            envelope: SignedOp,
        ) -> crate::apps::collab_inbound::Delivery {
            use crate::apps::collab_inbound::{RemoteDocument, deliver};
            use tilo_action_policy::{MemberAccess, Role, WorkspacePolicy};
            let editor = tilo_action_policy::PersonId::from_root(EDITOR);
            let mut policy = WorkspacePolicy::new(&self.workspace, "owner");
            policy
                .members
                .insert(editor.as_str().to_owned(), Role::Editor);
            deliver(
                &self.channels,
                &self.fanout,
                RemoteDocument {
                    app_id: CHAT.into(),
                    cluster_id: self.workspace.clone(),
                    document_id: record.into(),
                    author: MemberAccess::person(policy, editor),
                    replayed: false,
                    envelope,
                },
            )
        }
    }

    /// The root key of the person whose device a peer's edits are signed by.
    const EDITOR: [u8; 32] = [5; 32];

    /// A peer's `sequence`-th edit of `record` in `workspace`, setting its title, sealed as a
    /// workspace's every write is.
    fn edit(workspace: &str, record: &str, sequence: u64, title: &str) -> SignedOp {
        use collab_types::{
            ContentRetention, EncryptionMetadata, OpKind, field_edit_mutation, sign_op,
        };
        let key = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
        let (mutation, _) = field_edit_mutation("note", "title", title, &EDITOR).unwrap();
        let mut op = SignedOp::new_unsigned_with_encryption(
            mutation.encode().unwrap(),
            document_artifact_id(CHAT, workspace, record),
            Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
            document_attestation_id(CHAT, workspace, record),
            OpKind::Change,
            tilo_clock::wall(),
            sequence,
            EncryptionMetadata::content_key(1, ContentRetention::Delta),
        );
        sign_op(&mut op, &key).unwrap();
        op
    }

    /// A peer's write of a record reaches the window open on the app. A window that keeps no
    /// document of its own and reads the workspace's store a record at a time (a tilolang app's
    /// window) is told which record moved, on the app's route, for every op — a later edit of a
    /// record it already read, and the document a type's resources live in, not only the first
    /// write of a record of a tree. A window that holds the documents is told a part a peer wrote
    /// once, when it first arrives, which is what opens it.
    #[test]
    fn a_peer_written_record_reaches_the_apps_open_window() {
        use crate::apps::collab_inbound::Delivery;
        let workspace = uuid::Uuid::from_u128(7).to_string();
        let value = part(0);
        let root = "root-note".to_owned();
        let mut open = peer_writes(&workspace, &value);
        assert_eq!(
            open.deliver(&value, edit(&workspace, &value, 1, "hello")),
            Delivery::Done
        );
        assert_eq!(moved(&mut open.window), [vec![value.clone()]]);
        assert_eq!(
            open.announced.lock().unwrap().as_slice(),
            &[(open.app.clone(), vec![value.clone()])],
            "a window that holds the documents opens the part a peer wrote, told once"
        );
        assert_eq!(
            open.deliver(&value, edit(&workspace, &value, 2, "hello again")),
            Delivery::Done,
            "a later edit of a record the window opened is delivered to it"
        );
        assert_eq!(
            moved(&mut open.window),
            [vec![value]],
            "every op of a record tells the window it moved, not only the first"
        );
        assert_eq!(
            open.deliver(&root, edit(&workspace, &root, 1, "a note")),
            Delivery::Done,
            "a peer wrote the document a type's resources live in"
        );
        assert_eq!(
            moved(&mut open.window),
            [vec![root]],
            "the window is told the root document, on the app's route"
        );
        assert!(
            moved(&mut open.elsewhere).is_empty(),
            "another workspace's window is told nothing"
        );
        assert!(
            moved(&mut open.record_route).is_empty(),
            "a record's subscriber holds the document itself and is told no move"
        );
        assert_eq!(
            open.announced.lock().unwrap().len(),
            1,
            "only the part's first op is an arrival a document window opens, not every edit"
        );
    }
}
