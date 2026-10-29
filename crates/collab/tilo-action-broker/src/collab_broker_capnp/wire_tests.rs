#![allow(
    clippy::significant_drop_tightening,
    reason = "each test's room outlives the endpoints it serves; dropping it early would close them"
)]

use super::*;
use crate::collab_outbound::recording::{RecordingSink, Sent};
use crate::test_support::WorkspaceTree;
use collab_types::{PeerPresence, PeerPresencePayload, sign_op};
use ed25519_dalek::SigningKey;
use std::time::SystemTime;

struct Room {
    state: Arc<CapnpBrokerState>,
    route: CrdtRoute,
    sink: Arc<RecordingSink>,
    membership: crate::test_support::WorkspaceTreeHandle,
    directories: Rc<LocalActionDirectoryRegistry>,
    fanout: Arc<FanoutState>,
}

impl Room {
    /// A room whose members are the roots seeded with `members`.
    fn new(members: &[u8]) -> Self {
        let mut workspace =
            crate::test_support::Workspace::new(crate::test_support::WorkspaceId(9), "Wire");
        for seed in members {
            workspace.admit(principal(*seed), crate::test_support::Role::Editor);
        }
        let route = CrdtRoute::record(
            "so.tilo.notes",
            workspace.room().to_string(),
            "meeting-notes",
        );
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let membership = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let sink = Arc::new(RecordingSink::default());
        let (state, bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::new(sink.clone()),
            Some(membership.clone()),
            Some(Arc::new(
                members
                    .iter()
                    .map(|seed| device(*seed).verifying_key().to_bytes())
                    .collect(),
            )),
        );
        fanout.attach_capnp(state.clone());
        state.ensure_channel(&route.channel_key());
        Self {
            state,
            route,
            sink,
            membership,
            directories: Rc::new(LocalActionDirectoryRegistry::new(bridge_rx)),
            fanout,
        }
    }

    /// An endpoint of `seed`'s person on its own wire connection, holding
    /// the room's channel.
    fn endpoint(&self, seed: u8) -> Endpoint {
        let (connection, outgoing, channel) = self.connection(seed);
        Endpoint::serve(connection, outgoing, channel)
    }

    /// A connection of `seed`'s person holding the room's channel, not yet
    /// served.
    fn connection(
        &self,
        seed: u8,
    ) -> (
        Rc<WireConnection>,
        mpsc::UnboundedReceiver<HostFrame>,
        Handle,
    ) {
        let root = SigningKey::from_bytes(&[seed; 32]);
        let admitted = admit_verified_join(
            &self.membership.read(),
            &self.route,
            VerifiedJoinIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: device(seed).verifying_key().to_bytes(),
                capped: false,
            },
        )
        .unwrap();
        let (connection, outgoing) = WireConnection::new(None);
        let hold = self.state.next_peer_id();
        let channel = connection.open(ChannelSession::new(Rc::new(CapnpChannelHandle {
            person_chat_views: RefCell::default(),
            committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
            state: self.state.clone(),
            directories: self.directories.clone(),
            channel_id: self.route.channel_key(),
            peer_id: hold,
            connection_id: hold,
            verified_identity: admitted,
            record_kind: None,
            route_access: RouteAccess::Own,
            lease: None,
            _hold: None,
        })));
        (connection, outgoing, channel)
    }

    /// A connection that joins channels itself, served to an endpoint
    /// holding none yet.
    fn joining_endpoint(&self) -> Endpoint {
        let peer_id = self.fanout.next_peer_id();
        let service = Rc::new(CapnpCollabService {
            worker: None,
            app_worker: None,
            state: self.state.clone(),
            directories: self.directories.clone(),
            peer_id,
            lease: Rc::new(ConnectionLease {
                local_media: None,
                state: self.state.clone(),
                directories: self.directories.clone(),
                peer_id,
            }),
            holds: route_holds::ConnectionHolds::default(),
        });
        let (connection, outgoing) = WireConnection::new(Some(service));
        Endpoint::serve(connection, outgoing, Handle(0))
    }
}

fn device(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed.wrapping_add(100); 32])
}

fn principal(seed: u8) -> String {
    principal_id_for_root(
        SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .to_bytes(),
    )
}

struct Endpoint {
    channel: Handle,
    stream: tokio::net::UnixStream,
    next_id: u64,
    driver: tokio::task::JoinHandle<Result<(), wire_frame::FrameError>>,
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

impl Endpoint {
    fn serve(
        connection: Rc<WireConnection>,
        outgoing: mpsc::UnboundedReceiver<HostFrame>,
        channel: Handle,
    ) -> Self {
        let (host, endpoint) = tokio::net::UnixStream::pair().unwrap();
        let (reader, writer) = host.into_split();
        let driver = tokio::task::spawn_local(serve(connection, outgoing, reader, writer));
        Self {
            channel,
            stream: endpoint,
            next_id: 1,
            driver,
        }
    }

    async fn send(&mut self, request: Request) -> u64 {
        use tokio::io::AsyncWriteExt as _;
        let id = self.next_id;
        self.next_id = id.wrapping_add(1);
        FRAMING
            .write_value_async(&mut self.stream, &Call { id, request })
            .await
            .unwrap();
        self.stream.flush().await.unwrap();
        id
    }

    async fn frame(&mut self) -> HostFrame {
        tilo_test_env::within("the next host frame", async {
            FRAMING.read_value_async(&mut self.stream).await
        })
        .await
        .unwrap()
        .expect("the host keeps the connection open")
    }

    async fn call(&mut self, request: Request) -> Result<Reply, String> {
        let id = self.send(request).await;
        match self.frame().await {
            HostFrame::Reply {
                id: answered,
                result,
            } if answered == id => result,
            other => panic!("expected the reply to {id}, got {other:?}"),
        }
    }

    async fn publish(
        &mut self,
        key: &PresenceKey,
        payload: PeerPresencePayload,
    ) -> Result<Reply, String> {
        let channel = self.channel;
        self.call(Request::Publish {
            channel,
            publication: Publication::Presence(PeerPresence {
                key: postcard::to_stdvec(key).unwrap(),
                payload,
            }),
        })
        .await
    }
}

#[tokio::test(flavor = "current_thread")]
async fn presence_crosses_the_wire_attributed_and_replicated() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = Room::new(&[1, 2]);
            let mut watcher = room.endpoint(1);
            let mut author = room.endpoint(2);
            let channel = watcher.channel;
            let subscription = watcher.send(Request::Subscribe {
                channel,
                topic: Topic::Presence,
            })
            .await;
            assert_eq!(
                watcher.frame().await,
                HostFrame::Reply {
                    id: subscription,
                    result: Ok(Reply::Subscribed),
                }
            );

            let key = PresenceKey::surface_pointer("editor/document-7");
            assert_eq!(
                author
                    .publish(
                        &key,
                        PeerPresencePayload::Status {
                            text: "  Reviewing   Q2 ".into(),
                            automatic: false,
                        },
                    )
                    .await,
                Ok(Reply::Done)
            );
            let HostFrame::Event {
                subscription: delivered,
                event: Event::Presence { user_id, presence },
            } = watcher.frame().await
            else {
                panic!("the watcher receives the author's presence");
            };
            assert_eq!(delivered, subscription);
            assert_eq!(user_id, principal(2), "the host attributes the update");
            assert_eq!(postcard::from_bytes::<PresenceKey>(&presence.key).unwrap(), key);
            assert_eq!(
                presence.payload,
                PeerPresencePayload::Status {
                    text: "Reviewing Q2".into(),
                    automatic: false,
                },
                "the broker trims a status before it travels"
            );
            assert!(
                room.sink.snapshot().iter().any(|sent| matches!(
                    sent,
                    Sent::Presence { presence, .. } if presence.payload == PeerPresencePayload::Status {
                        text: "Reviewing Q2".into(),
                        automatic: false,
                    }
                )),
                "the same update is replicated to the room's peers"
            );

            assert!(
                author
                    .publish(
                        &key,
                        PeerPresencePayload::Availability {
                            state: "away".into(),
                            call: None,
                        },
                    )
                    .await
                    .is_err(),
                "an app never speaks for the desktop's presence"
            );

            assert_eq!(
                watcher.call(Request::Unsubscribe { subscription }).await,
                Ok(Reply::Done)
            );
            author
                .publish(&key, PeerPresencePayload::Arrived)
                .await
                .unwrap();
            let channel = watcher.channel;
            assert_eq!(
                watcher.call(Request::Close { handle: channel }).await,
                Ok(Reply::Done),
                "nothing more arrives on an ended subscription before the next reply"
            );
            assert!(
                watcher
                    .publish(&key, PeerPresencePayload::Left)
                    .await
                    .is_err(),
                "a closed handle names nothing"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_signed_change_crosses_the_wire_to_the_channel_subscribers() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = Room::new(&[4, 5]);
            let mut watcher = room.endpoint(4);
            let mut author = room.endpoint(5);
            let channel = watcher.channel;
            let subscription = watcher
                .send(Request::Subscribe {
                    channel,
                    topic: Topic::Ops,
                })
                .await;
            assert_eq!(
                watcher.frame().await,
                HostFrame::Reply {
                    id: subscription,
                    result: Ok(Reply::Subscribed),
                }
            );

            let route = &room.route;
            let mut op = SignedOp::new_unsigned(
                DocumentMutation::Snapshot(b"automerge snapshot".to_vec())
                    .encode()
                    .unwrap(),
                document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id),
                collab_types::Ed25519PublicKey::from_bytes(device(5).verifying_key().to_bytes()),
                document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id),
                collab_types::OpKind::Change,
                SystemTime::now(),
                1,
            );
            sign_op(&mut op, &device(5)).unwrap();
            let channel = author.channel;
            assert_eq!(
                author
                    .call(Request::Publish {
                        channel,
                        publication: Publication::Document(Box::new(op.clone())),
                    })
                    .await,
                Ok(Reply::Done)
            );
            let HostFrame::Event {
                subscription: delivered,
                event:
                    Event::Op {
                        op: bytes,
                        from_user_id,
                        ..
                    },
            } = watcher.frame().await
            else {
                panic!("the watcher receives the change");
            };
            assert_eq!(delivered, subscription);
            assert_eq!(postcard::from_bytes::<SignedOp>(&bytes).unwrap(), op);
            assert!(!from_user_id.is_empty());

            let channel = watcher.channel;
            assert!(matches!(
                watcher
                    .call(Request::Read {
                        channel,
                        read: Read::History {
                            artifact: op.artifact,
                            before_seq: u64::MAX,
                            max: 16,
                        },
                    })
                    .await,
                Ok(Reply::History { .. })
            ));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_late_joiner_asks_a_context_subscriber_for_the_document() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = Room::new(&[6, 8]);
            let mut holder = room.endpoint(6);
            let mut joiner = room.endpoint(8);
            let channel = holder.channel;
            assert_eq!(
                holder
                    .call(Request::Subscribe {
                        channel,
                        topic: Topic::Context,
                    })
                    .await,
                Ok(Reply::Subscribed)
            );
            let channel = joiner.channel;
            assert_eq!(
                joiner
                    .call(Request::Read {
                        channel,
                        read: Read::Document,
                    })
                    .await,
                Ok(Reply::Done)
            );
            assert!(matches!(
                holder.frame().await,
                HostFrame::Event {
                    event: Event::Context(ContextUpdate::SnapshotRequested),
                    ..
                }
            ));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_join_opens_a_handle_the_endpoint_then_works_in() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = Room::new(&[3]);
            let mut endpoint = room.joining_endpoint();
            let channel = room.route.channel_key();
            assert_eq!(
                endpoint
                    .call(Request::Room {
                        channel: channel.clone(),
                        seen: None,
                    })
                    .await,
                Ok(Reply::Room(runtime_collab::RoomStatus::Ready))
            );
            let refused = endpoint
                .call(Request::Join(Join {
                    channel: channel.clone(),
                    identity: crate::collab_broker_capnp::tests::certified_join(
                        &room.route.cluster_id,
                        &SigningKey::from_bytes(&[3; 32]),
                        &device(4),
                    ),
                    participant: Participant::Person {
                        handle: String::new(),
                    },
                    record_kind: None,
                }))
                .await;
            assert!(
                refused.is_err(),
                "a device the room never enrolled joins nothing"
            );
            let Ok(Reply::Opened(handle)) = endpoint
                .call(Request::Join(Join {
                    channel,
                    identity: crate::collab_broker_capnp::tests::certified_join(
                        &room.route.cluster_id,
                        &SigningKey::from_bytes(&[3; 32]),
                        &device(3),
                    ),
                    participant: Participant::Person {
                        handle: "Ada".into(),
                    },
                    record_kind: None,
                }))
                .await
            else {
                panic!("the member's enrolled device joins");
            };
            endpoint.channel = handle;
            assert_eq!(
                endpoint
                    .publish(
                        &PresenceKey::surface_pointer("editor"),
                        PeerPresencePayload::Arrived,
                    )
                    .await,
                Ok(Reply::Done)
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_service_command_is_answered_or_refused_on_its_call() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = Room::new(&[9]);
            let mut endpoint = room.endpoint(9);
            let channel = endpoint.channel;
            for service in [
                Service::AudioOutput,
                Service::NativeMedia,
                Service::PersonChat,
                Service::Speaker,
                Service::Realtime,
                Service::NearbyTransfer,
                Service::WifiContinuity,
                Service::BrowserServiceSession,
                Service::ApprovedCheck,
                Service::LocalFile,
                Service::CommittedDocument,
                Service::RemoteAction,
            ] {
                let refused = endpoint
                    .call(Request::Command {
                        channel,
                        service,
                        body: b"not a command".to_vec(),
                        on_behalf_of: Vec::new(),
                    })
                    .await;
                assert!(refused.is_err(), "{service:?} refuses what it cannot read");
            }
            assert!(
                endpoint
                    .call(Request::Command {
                        channel: Handle(77),
                        service: Service::PersonChat,
                        body: Vec::new(),
                        on_behalf_of: Vec::new(),
                    })
                    .await
                    .is_err(),
                "a command names a channel the connection holds"
            );
            for report in [
                Report::Continuation {
                    schema_version: 1,
                    metadata: Vec::new(),
                },
                Report::ClearContinuation,
                Report::Handoff(Vec::new()),
                Report::SearchIndex(b"{}".to_vec()),
            ] {
                assert!(
                    endpoint
                        .call(Request::Publish {
                            channel,
                            publication: Publication::Report(report.clone()),
                        })
                        .await
                        .is_err(),
                    "{report:?}: a channel with no instance or durable room reports nothing"
                );
            }
            assert!(
                endpoint
                    .call(Request::Publish {
                        channel,
                        publication: Publication::Effect {
                            resource: "note:1".into(),
                            action: "archive".into(),
                            update: "{}".into(),
                        },
                    })
                    .await
                    .is_err(),
                "an effect names a terminal update of an action the app publishes"
            );
            for of in [
                Changes::Speakers,
                Changes::ChannelKeys,
                Changes::NearbyTransfers,
                Changes::Drives,
                Changes::Applications,
            ] {
                assert!(
                    endpoint
                        .call(Request::Changed {
                            channel,
                            of,
                            after: 0,
                        })
                        .await
                        .is_err(),
                    "{of:?}: a host without the service, or a route it is not for, waits on nothing"
                );
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn task_runs_and_their_calls_are_only_for_a_delegated_agent() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = Room::new(&[10]);
            let mut endpoint = room.endpoint(10);
            let channel = endpoint.channel;
            assert!(
                endpoint
                    .call(Request::Subscribe {
                        channel,
                        topic: Topic::TaskRuns,
                    })
                    .await
                    .is_err(),
                "task runs go only to a delegated agent in a room that runs tasks"
            );
            let step = TaskStep {
                peer_id: 0,
                provider: String::new(),
                resource: "note:1".into(),
                action: "archive".into(),
                argument: None,
                invocation: String::new(),
                wait_ms: 0,
                seen: None,
            };
            for call in [
                TaskCall::Report("{}".into()),
                TaskCall::Cancelled,
                TaskCall::Invoke(step),
            ] {
                assert!(
                    endpoint
                        .call(Request::Task {
                            channel,
                            run_id: 1,
                            lease: "lease".into(),
                            call: call.clone(),
                        })
                        .await
                        .is_err(),
                    "{call:?}: a person holds no task run"
                );
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_app_takes_its_invocations_and_the_catalogue_and_answers_only_its_own() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = Room::new(&[13]);
            let mut endpoint = room.endpoint(13);
            let channel = endpoint.channel;
            assert_eq!(
                endpoint
                    .call(Request::Subscribe {
                        channel,
                        topic: Topic::Invocations { stable_ids: true },
                    })
                    .await,
                Ok(Reply::Subscribed)
            );
            for call in [
                InvocationCall::Complete {
                    code: String::new(),
                    message: String::new(),
                },
                InvocationCall::Update("{}".into()),
                InvocationCall::Cancelled,
            ] {
                assert!(
                    endpoint
                        .call(Request::Invocation {
                            channel,
                            id: 404,
                            call: call.clone(),
                        })
                        .await
                        .is_err(),
                    "{call:?}: an invocation the app was never handed"
                );
            }
            assert_eq!(
                endpoint
                    .call(Request::Subscribe {
                        channel,
                        topic: Topic::Catalogue,
                    })
                    .await,
                Ok(Reply::Subscribed)
            );
            assert!(
                matches!(
                    endpoint.frame().await,
                    HostFrame::Event {
                        event: Event::Catalogue(_),
                        ..
                    }
                ),
                "the catalogue as it stands arrives at once"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_app_publishes_its_action_surface_whole_or_as_a_change() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = Room::new(&[14]);
            let mut endpoint = room.endpoint(14);
            let channel = endpoint.channel;
            let reset = serde_json::to_string(&runtime_collab::SurfaceDelta {
                reset: true,
                ..runtime_collab::SurfaceDelta::default()
            })
            .unwrap();
            for publication in [
                Publication::Actions("[]".into()),
                Publication::Surface(reset),
            ] {
                assert_eq!(
                    endpoint
                        .call(Request::Publish {
                            channel,
                            publication: publication.clone(),
                        })
                        .await,
                    Ok(Reply::Done),
                    "{publication:?}"
                );
            }
            for publication in [
                Publication::Actions("not actions".into()),
                Publication::Surface("{}".into()),
            ] {
                assert!(
                    endpoint
                        .call(Request::Publish {
                            channel,
                            publication: publication.clone(),
                        })
                        .await
                        .is_err(),
                    "{publication:?} does not decode"
                );
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_wire_client_calls_and_subscribes_against_the_host() {
    use runtime_collab::wire::client::{Client, Delivery};
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = Room::new(&[11, 12]);
            let connect = |seed: u8| {
                let (connection, outgoing, channel) = room.connection(seed);
                let (host, endpoint) = tokio::net::UnixStream::pair().unwrap();
                let (reader, writer) = host.into_split();
                tokio::task::spawn_local(serve(connection, outgoing, reader, writer));
                let (reader, writer) = endpoint.into_split();
                let (client, frames) = Client::new(reader, writer);
                tokio::task::spawn_local(frames);
                (client, channel)
            };
            let (watcher, watcher_channel) = connect(11);
            let (author, authored) = connect(12);
            let mut presence = watcher
                .subscribe(watcher_channel, Topic::Presence)
                .await
                .unwrap();
            let key = PresenceKey::surface_pointer("editor");
            assert_eq!(
                author
                    .call(Request::Publish {
                        channel: authored,
                        publication: Publication::Presence(PeerPresence {
                            key: postcard::to_stdvec(&key).unwrap(),
                            payload: PeerPresencePayload::Arrived,
                        }),
                    })
                    .await,
                Ok(Reply::Done)
            );
            let Some(Delivery::Event(Event::Presence { user_id, .. })) = presence.next().await
            else {
                panic!("the client routes the subscription's event");
            };
            assert_eq!(user_id, principal(12));
            assert!(
                watcher
                    .call(Request::Close { handle: Handle(99) })
                    .await
                    .is_err(),
                "a refusal reaches the call that caused it"
            );
            watcher.unsubscribe(presence).await.unwrap();
        })
        .await;
}
