//! Joins that name a participant: an Agent's exact identity, and a
//! Person's presentation handle that never becomes their principal.
use super::*;

struct ParticipantJoinFixture {
    state: Arc<CapnpBrokerState>,
    fanout: Arc<FanoutState>,
    client: runtime_collab::CollabClient,
    presentation: Vec<u8>,
    route: CrdtRoute,
    principal: String,
}

impl ParticipantJoinFixture {
    fn new(agents: &[String]) -> Self {
        let root = SigningKey::from_bytes(&[101; 32]);
        let device = SigningKey::from_bytes(&[102; 32]);
        let principal = principal_id_for_root(root.verifying_key().to_bytes());
        let mut workspace = crate::test_support::Workspace::new(
            crate::test_support::WorkspaceId(101),
            "Exact Agent identity",
        );
        workspace.set_owner(principal.clone());
        let route = CrdtRoute::cluster(workspace.room().to_string());
        for agent in agents {
            workspace.admit_agent(agent, crate::test_support::KindSet::READ);
        }
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let fanout = Arc::new(FanoutState::new());
        let (state, bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(tree.shared()),
            Some(Arc::new(vec![device.verifying_key().to_bytes()])),
        );
        let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge_rx));
        let peer_id = fanout.next_peer_id();
        let lease = Rc::new(ConnectionLease {
            local_media: None,
            state: Arc::clone(&state),
            directories: Rc::clone(&directories),
            peer_id,
        });
        let raw: collab_service::Client = capnp_rpc::new_client(CapnpCollabService {
            worker: None,
            app_worker: None,
            state: Arc::clone(&state),
            directories,
            peer_id,
            lease,
            holds: route_holds::ConnectionHolds::default(),
        });
        let presentation = certified_join(&route.cluster_id, &root, &device)
            .encode()
            .unwrap();
        Self {
            state,
            fanout,
            client: runtime_collab::CollabClient::from_raw(raw),
            presentation,
            route,
            principal,
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn signed_agent_join_preserves_exact_ascii_and_multibyte_identity() {
    let ascii = "a".repeat(128);
    let multibyte = "\u{e9}".repeat(64);
    let fixture = ParticipantJoinFixture::new(&[
        ascii.clone(),
        multibyte.clone(),
        // The old display-name truncation would silently select this
        // different, also admitted Agent rather than reject the join.
        "a".repeat(64),
    ]);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let _subscription = fixture.fanout.subscribe_rooms(
        vec![fixture.route.cluster_id.clone()],
        Box::new(move |event, _| {
            event_tx.send(event.clone()).unwrap();
        }),
    );
    let key = PresenceKey::Resource {
        app: "so.tilo.notes".into(),
        id: "note:draft".into(),
        kind: "note".into(),
    };
    let key_bytes = postcard::to_stdvec(&key).unwrap();
    for expected in [ascii, multibyte] {
        let input = format!(" {} ", expected.to_uppercase());
        let channel = fixture
            .client
            .join_channel_as_kind(
                &fixture.route.channel_key(),
                &fixture.presentation,
                &input,
                "agent",
                "",
            )
            .await
            .expect("certified join preserves the complete canonical Agent ID");
        channel
            .presence()
            .await
            .unwrap()
            .publish_arrived(&key_bytes)
            .await
            .unwrap();
        let event = event_rx.try_recv().unwrap();
        assert!(matches!(event,
            CollabEvent::RemotePresenceUpdate { user_id, key: delivered, payload: PresencePayload::Arrived }
                if user_id == format!("agent:{expected}") && delivered == key));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn signed_agent_join_refuses_invalid_ids_without_display_prefix_fallback() {
    // The broker serves joins on its LocalSet: a person's join starts its
    // app-authorship admission there (`spawn_local`).
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = ParticipantJoinFixture::new(&[
                "a".repeat(64),
                "a".repeat(128),
                "\u{e9}".repeat(64),
            ]);
            for invalid in [
                "a".repeat(129),
                "\u{e9}".repeat(65),
                // Lowercase expands this input from 86 to 129 UTF-8 bytes.
                "\u{130}".repeat(43),
                format!("{}\nx", "a".repeat(64)),
                String::new(),
            ] {
                assert!(
                    fixture
                        .client
                        .join_channel_as_kind(
                            &fixture.route.channel_key(),
                            &fixture.presentation,
                            &invalid,
                            "agent",
                            "",
                        )
                        .await
                        .is_err(),
                    "invalid Agent IDs must refuse instead of selecting an enrolled prefix"
                );
                assert!(fixture.state.channels.lock().unwrap().is_empty());
            }
            // The same long string is still a Person's bounded presentation
            // handle: it never becomes that Person's certified principal.
            let person = fixture
                .client
                .join_channel_as_kind(
                    &fixture.route.channel_key(),
                    &fixture.presentation,
                    &"a".repeat(129),
                    "person",
                    "",
                )
                .await
                .unwrap();
            let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
            let _subscription = fixture.fanout.subscribe_rooms(
                vec![fixture.route.cluster_id.clone()],
                Box::new(move |event, _| {
                    event_tx.send(event.clone()).unwrap();
                }),
            );
            person
                .presence()
                .await
                .unwrap()
                .publish_arrived(&postcard::to_stdvec(&PresenceKey::pointer_default()).unwrap())
                .await
                .unwrap();
            assert!(matches!(event_rx.try_recv().unwrap(),
                CollabEvent::RemotePresenceUpdate { user_id, payload: PresencePayload::Arrived, .. }
                    if user_id == fixture.principal));
        })
        .await;
}
