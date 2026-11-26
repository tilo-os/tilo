//! Connections whose launch names a member: an Agent's exact identity, and a
//! Person's connection, whose actor is always their certified one.
use super::*;

struct MemberJoinFixture {
    state: Arc<AppBrokerState>,
    fanout: Arc<FanoutState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    certificate: tilo_user_identity::DeviceCert,
    route: CrdtRoute,
    actor: String,
}

impl MemberJoinFixture {
    fn new(agents: &[String]) -> Self {
        let root = SigningKey::from_bytes(&[101; 32]);
        let device = SigningKey::from_bytes(&[102; 32]);
        let actor = person_id_for_root(root.verifying_key().to_bytes());
        let mut workspace = crate::test_support::Workspace::new(
            crate::test_support::WorkspaceId::from_u128(101),
            "Exact Agent identity",
        );
        workspace.policy.set_owner(actor.clone());
        let route = CrdtRoute::cluster(workspace.id().to_string());
        for agent in agents {
            // The owner's own agents: their certified device runs them.
            use crate::test_support::{Grantee, Grants as _, KindSet, TreePath};
            let theirs = Grantee::agent(agent.as_str(), actor.as_str());
            workspace.allow(theirs, &TreePath::root(), KindSet::READ);
        }
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let fanout = Arc::new(FanoutState::new());
        let (state, bridge_rx) = AppBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(tree.shared()),
            Some(Arc::new(vec![device.verifying_key().to_bytes()])),
        );
        let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge_rx));
        Self {
            state,
            fanout,
            directories,
            certificate: device_certificate(&root, &device),
            route,
            actor,
        }
    }

    /// The record of the fixture's route, on a connection whose launch names `member`.
    async fn opened(
        &self,
        member: Option<&str>,
    ) -> Result<runtime_collab::RecordHandle, runtime_portal::NotAvailable> {
        self.opened_as(self.certificate.clone(), member).await
    }

    /// The record of the fixture's route, on a connection admitted under `certificate`.
    async fn opened_as(
        &self,
        certificate: tilo_user_identity::DeviceCert,
        member: Option<&str>,
    ) -> Result<runtime_collab::RecordHandle, runtime_portal::NotAvailable> {
        let (client, _server, _client) = certified_rpc(
            &self.state,
            &self.directories,
            self.fanout.next_peer_id(),
            certificate,
            member,
        );
        opened(&client, &self.route.channel_key()).await
    }
}

#[tokio::test(flavor = "current_thread")]
async fn signed_agent_join_preserves_exact_ascii_and_multibyte_identity() {
    // The wire connection is served on the test's LocalSet.
    tokio::task::LocalSet::new()
        .run_until(async {
            let ascii = "a".repeat(128);
            let multibyte = "\u{e9}".repeat(64);
            let fixture = MemberJoinFixture::new(&[
                ascii.clone(),
                multibyte.clone(),
                // The old display-name truncation would silently select this
                // different, also admitted Agent rather than reject the join.
                "a".repeat(64),
            ]);
            let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
            let _subscription = fixture.fanout.subscribe_workspaces(
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
            let frame = collab_types::presence::Presence::app(
                "",
                fixture.route.cluster_id.clone(),
                key.clone(),
                collab_types::presence::Payload::Arrived,
            );
            for expected in [ascii, multibyte] {
                let input = format!(" {} ", expected.to_uppercase());
                let channel = fixture
                    .opened(Some(&input))
                    .await
                    .expect("certified join preserves the complete canonical Agent ID");
                channel.say(frame.clone()).await.unwrap();
                // The connection before this one left what it said when it closed.
                let event = loop {
                    match event_rx.recv().await.unwrap() {
                        CollabEvent::RemotePresenceUpdate {
                            payload: PresencePayload::Left,
                            ..
                        } => {}
                        event => break event,
                    }
                };
                assert!(matches!(event,
                    CollabEvent::RemotePresenceUpdate { member, key: delivered, payload: PresencePayload::Arrived }
                        if member == collab_types::MemberId::agent(expected, fixture.actor.as_str()) && delivered == key));
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn signed_agent_join_refuses_invalid_ids_without_display_prefix_fallback() {
    // The broker serves joins on its LocalSet: a person's join starts its
    // app-authorship admission there (`spawn_local`).
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture =
                MemberJoinFixture::new(&["a".repeat(64), "a".repeat(128), "\u{e9}".repeat(64)]);
            for invalid in [
                "a".repeat(129),
                "\u{e9}".repeat(65),
                // Lowercase expands this input from 86 to 129 UTF-8 bytes.
                "\u{130}".repeat(43),
                format!("{}\nx", "a".repeat(64)),
                String::new(),
            ] {
                assert!(
                    fixture.opened(Some(&invalid)).await.is_err(),
                    "invalid Agent IDs must refuse instead of selecting an enrolled prefix"
                );
                assert!(fixture.state.channels.lock().unwrap().is_empty());
            }
            // A connection naming no agent acts as the Person's certified actor.
            let person = fixture.opened(None).await.unwrap();
            let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
            let _subscription = fixture.fanout.subscribe_workspaces(
                vec![fixture.route.cluster_id.clone()],
                Box::new(move |event, _| {
                    event_tx.send(event.clone()).unwrap();
                }),
            );
            person
                .say(collab_types::presence::Presence::app(
                    "",
                    fixture.route.cluster_id.clone(),
                    PresenceKey::pointer_default(),
                    collab_types::presence::Payload::Arrived,
                ))
                .await
                .unwrap();
            assert!(matches!(event_rx.try_recv().unwrap(),
                CollabEvent::RemotePresenceUpdate { member, payload: PresencePayload::Arrived, .. }
                    if member == collab_types::MemberId::person(fixture.actor.clone())));
        })
        .await;
}

/// A connection is admitted to the workspace's record only under the certificate of a device this
/// host trusts, which the workspace's person certified: an app connection of another person's
/// device is refused, and so is one under a certificate this device does not hold (another device
/// of the same person, or this device's key under a certificate its person never signed).
#[tokio::test(flavor = "current_thread")]
async fn a_connection_under_a_certificate_this_device_does_not_hold_is_refused() {
    // The broker serves the connections on the test's LocalSet.
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = MemberJoinFixture::new(&[]);
            let root = SigningKey::from_bytes(&[101; 32]);
            let device = SigningKey::from_bytes(&[102; 32]);
            let another_person = device_certificate(
                &SigningKey::from_bytes(&[103; 32]),
                &SigningKey::from_bytes(&[104; 32]),
            );
            let another_device = device_certificate(&root, &SigningKey::from_bytes(&[105; 32]));
            let mut unsigned = device_certificate(&root, &device);
            unsigned.signature =
                device_certificate(&SigningKey::from_bytes(&[103; 32]), &device).signature;
            for (certificate, whose) in [
                (another_person, "another person's device"),
                (another_device, "another of the person's devices"),
                (unsigned, "a certificate the person never signed"),
            ] {
                let refused = fixture.opened_as(certificate, None).await;
                assert!(
                    refused.is_err(),
                    "a connection under {whose}'s certificate is refused the record"
                );
                assert!(
                    fixture.state.channels.lock().unwrap().is_empty(),
                    "nothing is held for a connection under {whose}'s certificate"
                );
            }
            fixture
                .opened(None)
                .await
                .expect("the device's own certificate opens the record");
        })
        .await;
}
