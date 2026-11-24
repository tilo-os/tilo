#![allow(
    clippy::significant_drop_tightening,
    reason = "each test's workspace outlives the endpoints it serves; dropping it early would close them"
)]

use super::*;
use crate::apps::collab_outbound::recording::{RecordingSink, Sent};
use crate::apps::test_support::WorkspaceTree;
use collab_types::{PeerPresence, PeerPresencePayload, sign_op};
use ed25519_dalek::SigningKey;
use std::sync::atomic::Ordering;
use std::time::SystemTime;

struct Workspace {
    state: Arc<AppBrokerState>,
    route: CrdtRoute,
    sink: Arc<RecordingSink>,
    membership: crate::apps::test_support::WorkspaceTreeHandle,
    directories: Rc<LocalActionDirectoryRegistry>,
    fanout: Arc<FanoutState>,
}

impl Workspace {
    /// A workspace whose members are the roots seeded with `members`.
    fn new(members: &[u8]) -> Self {
        Self::serving(members, |services| services)
    }

    /// A workspace like [`Self::new`]'s, served what `serve` adds to its services.
    fn serving(members: &[u8], serve: impl FnOnce(ProcessServices) -> ProcessServices) -> Self {
        let mut workspace = crate::apps::test_support::Workspace::new(
            crate::apps::test_support::WorkspaceId::from_u128(9),
            "Wire",
        );
        for seed in members {
            workspace.admit(actor(*seed), crate::apps::test_support::Role::Editor);
        }
        let route = CrdtRoute::record("so.tilo.notes", workspace.id().to_string(), "meeting-notes");
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let membership = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let sink = Arc::new(RecordingSink::default());
        let (state, bridge_rx) = AppBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::new(sink.clone()),
            Some(membership.clone().authority()),
            Some(Arc::new(
                members
                    .iter()
                    .map(|seed| device(*seed).verifying_key().to_bytes())
                    .collect(),
            )),
            None,
            serve(membership.services()),
        );
        fanout.attach_app_broker(state.clone());
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
    /// the workspace's route.
    fn endpoint(&self, seed: u8) -> Endpoint {
        let (connection, outgoing, channel) = self.connection(seed, RouteAccess::Own);
        Endpoint::serve(connection, outgoing, channel)
    }

    /// A connection of `seed`'s person holding the workspace's route, not yet
    /// served.
    fn connection(
        &self,
        seed: u8,
        route_access: RouteAccess,
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
        let channel = connection.open(ChannelSession::new(Rc::new(JoinedChannel {
            committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
            state: self.state.clone(),
            directories: self.directories.clone(),
            channel_id: self.route.channel_key(),
            peer_id: hold,
            connection_id: hold,
            verified_identity: admitted,
            resource_type: None,
            route_access,
            lease: None,
            _hold: None,
        })));
        (connection, outgoing, channel)
    }

    /// A connection that joins routes itself, served to an endpoint
    /// holding none yet.
    fn joining_endpoint(&self) -> Endpoint {
        let peer_id = self.fanout.next_peer_id();
        let service = Rc::new(AppConnection {
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

fn actor(seed: u8) -> String {
    person_id_for_root(
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

    async fn send(&mut self, request: Operation) -> u64 {
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

    async fn call(&mut self, request: Operation) -> Result<Reply, String> {
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
        self.call(Operation::Publish {
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
            let workspace = Workspace::new(&[1, 2]);
            let mut watcher = workspace.endpoint(1);
            let mut author = workspace.endpoint(2);
            let channel = watcher.channel;
            let subscription = watcher.send(Operation::Subscribe {
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
                event: Event::Presence { member, presence },
            } = watcher.frame().await
            else {
                panic!("the watcher receives the author's presence");
            };
            assert_eq!(delivered, subscription);
            assert_eq!(member, collab_types::MemberId::person(actor(2)), "attributed");
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
                workspace.sink.snapshot().iter().any(|sent| matches!(
                    sent,
                    Sent::Presence { presence, .. } if presence.payload == PeerPresencePayload::Status {
                        text: "Reviewing Q2".into(),
                        automatic: false,
                    }
                )),
                "the same update is replicated to the workspace's peers"
            );

            assert!(
                author
                    .publish(
                        &key,
                        PeerPresencePayload::Availability {
                            state: "away".into(),
                            call: None,
                            camera: false,
                            screen: false,
                        },
                    )
                    .await
                    .is_err(),
                "an app never speaks for the desktop's presence"
            );

            assert_eq!(
                watcher.call(Operation::Unsubscribe { subscription }).await,
                Ok(Reply::Done)
            );
            author
                .publish(&key, PeerPresencePayload::Arrived)
                .await
                .unwrap();
            let channel = watcher.channel;
            assert_eq!(
                watcher.call(Operation::Close { handle: channel }).await,
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
            let workspace = Workspace::new(&[4, 5]);
            let mut watcher = workspace.endpoint(4);
            let mut author = workspace.endpoint(5);
            let channel = watcher.channel;
            let subscription = watcher
                .send(Operation::Subscribe {
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

            let route = &workspace.route;
            let mut op = SignedOp::new_unsigned(
                DocumentMutation::Snapshot(b"automerge snapshot".to_vec())
                    .encode()
                    .unwrap(),
                document_artifact_id(&route.app_id, &route.cluster_id, &route.document_id),
                collab_types::Ed25519PublicKey::from_bytes(device(5).verifying_key().to_bytes()),
                document_attestation_id(&route.app_id, &route.cluster_id, &route.document_id),
                collab_types::OpKind::Change,
                SystemTime::now(),
                1,
            );
            sign_op(&mut op, &device(5)).unwrap();
            let channel = author.channel;
            assert_eq!(
                author
                    .call(Operation::Publish {
                        channel,
                        publication: Publication::Document(Box::new(op.clone())),
                    })
                    .await,
                Ok(Reply::Done)
            );
            let HostFrame::Event {
                subscription: delivered,
                event: Event::Op { op: bytes, .. },
            } = watcher.frame().await
            else {
                panic!("the watcher receives the change");
            };
            assert_eq!(delivered, subscription);
            assert_eq!(postcard::from_bytes::<SignedOp>(&bytes).unwrap(), op);

            let channel = watcher.channel;
            assert!(matches!(
                watcher
                    .call(Operation::Read {
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
            let workspace = Workspace::new(&[6, 8]);
            let mut holder = workspace.endpoint(6);
            let mut joiner = workspace.endpoint(8);
            let channel = holder.channel;
            assert_eq!(
                holder
                    .call(Operation::Subscribe {
                        channel,
                        topic: Topic::Context,
                    })
                    .await,
                Ok(Reply::Subscribed)
            );
            let channel = joiner.channel;
            assert_eq!(
                joiner
                    .call(Operation::Read {
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
            let workspace = Workspace::new(&[3]);
            let mut endpoint = workspace.joining_endpoint();
            let channel = workspace.route.channel_key();
            assert_eq!(
                endpoint
                    .call(Operation::Workspace {
                        channel: channel.clone(),
                        seen: None,
                    })
                    .await,
                Ok(Reply::Workspace(runtime_collab::WorkspaceStatus::Ready))
            );
            let refused = endpoint
                .call(Operation::Join(Join {
                    channel: channel.clone(),
                    identity: crate::apps::collab_app_broker::tests::certified_join(
                        &workspace.route.cluster_id,
                        &SigningKey::from_bytes(&[3; 32]),
                        &device(4),
                    )
                    .encode()
                    .unwrap(),
                    member: None,
                    handle: String::new(),
                    resource_type: None,
                }))
                .await;
            assert!(
                refused.is_err(),
                "a device the workspace never enrolled joins nothing"
            );
            let Ok(Reply::Opened(handle)) = endpoint
                .call(Operation::Join(Join {
                    channel,
                    identity: crate::apps::collab_app_broker::tests::certified_join(
                        &workspace.route.cluster_id,
                        &SigningKey::from_bytes(&[3; 32]),
                        &device(3),
                    )
                    .encode()
                    .unwrap(),
                    member: None,
                    handle: "Ada".into(),
                    resource_type: None,
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

/// A store that keeps the app and workspace each command came from.
#[derive(Default)]
struct CountingStore(
    Mutex<Vec<(String, String)>>,
    /// The open queries, held so their apps keep them.
    Mutex<Vec<tokio::sync::mpsc::UnboundedSender<tilo_resource_store::Page>>>,
);

impl crate::apps::services::ResourceServices for CountingStore {
    fn command(
        &self,
        context: &crate::apps::services::ResourceRequestContext,
        command: tilo_resource_store::wire::Command,
    ) -> crate::apps::services::ResourceAnswer {
        let mut asked = self.0.lock().unwrap();
        asked.push((
            context.app_id().to_owned(),
            context.workspace_id().to_owned(),
        ));
        let answer = match command {
            tilo_resource_store::wire::Command::Count { .. } => {
                Ok(tilo_resource_store::wire::Reply::Count(asked.len()))
            }
            _ => Ok(tilo_resource_store::wire::Reply::Written),
        };
        drop(asked);
        Box::pin(std::future::ready(answer))
    }

    fn open(
        &self,
        _: &crate::apps::services::ResourceRequestContext,
        query: tilo_resource_store::Query,
    ) -> crate::apps::services::ResourcePages {
        // A first page of none, then whatever the test sends to the query's kind.
        let (pages, receiver) = tokio::sync::mpsc::unbounded_channel();
        let _ = pages.send(tilo_resource_store::Page::default());
        drop(query);
        self.1.lock().unwrap().push(pages);
        Box::pin(std::future::ready(Ok(receiver)))
    }
}

/// An app holds a query open over its route: the store's first page and each later one reach
/// it as events of the subscription.
#[tokio::test(flavor = "current_thread")]
async fn an_apps_open_query_sends_its_pages_over_its_route() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let store = Arc::new(CountingStore::default());
            let served = Arc::clone(&store);
            let workspace = Workspace::serving(&[9], move |services| ProcessServices {
                resources: Some(served),
                ..services
            });
            let mut app = workspace.endpoint(9);
            let channel = app.channel;
            let query =
                serde_json::to_vec(&tilo_resource_store::Query::of("tasks.issue", 10)).unwrap();
            let subscription = app
                .send(Operation::Subscribe {
                    channel,
                    topic: Topic::Resources(query),
                })
                .await;
            assert_eq!(
                app.frame().await,
                HostFrame::Reply {
                    id: subscription,
                    result: Ok(Reply::Subscribed),
                }
            );
            for _ in 0..2 {
                let HostFrame::Event {
                    subscription: of,
                    event: Event::Resources(page),
                } = app.frame().await
                else {
                    panic!("a page of the query arrives");
                };
                assert_eq!(of, subscription);
                let page: tilo_resource_store::Page = serde_json::from_slice(&page).unwrap();
                assert!(page.resources.is_empty());
                // The store's next page, as a change would send it.
                let sent = store.1.lock().unwrap()[0].send(tilo_resource_store::Page::default());
                sent.unwrap();
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_app_reads_through_its_certified_route_but_cannot_write_through_a_read_only_one() {
    use tilo_resource_store::wire::Command;

    tokio::task::LocalSet::new()
        .run_until(async {
            let store = Arc::new(CountingStore::default());
            let served = Arc::clone(&store);
            let workspace = Workspace::serving(&[9], move |services| ProcessServices {
                resources: Some(served),
                ..services
            });
            let (connection, outgoing, handle) = workspace.connection(9, RouteAccess::ReadOnly);
            let mut endpoint = Endpoint::serve(connection, outgoing, handle);
            let ask = |handle, command: &Command, on_behalf_of| Operation::Command {
                channel: handle,
                service: Service::Resources,
                body: serde_json::to_vec(command).unwrap(),
                on_behalf_of,
            };

            let answer = endpoint
                .call(ask(
                    handle,
                    &Command::Count {
                        kind: "tasks.issue".to_owned(),
                        filters: Vec::new(),
                    },
                    Vec::new(),
                ))
                .await
                .unwrap();
            let Reply::Answer(bytes) = answer else {
                panic!("a command is answered: {answer:?}");
            };
            let reply: tilo_resource_store::wire::Reply = serde_json::from_slice(&bytes).unwrap();
            assert!(matches!(reply, tilo_resource_store::wire::Reply::Count(1)));

            let write = Command::Remove {
                kind: "notes.note".to_owned(),
                id: "meeting-notes".to_owned(),
                stamp: tilo_resource_store::Stamp::default(),
            };
            let denied = endpoint
                .call(ask(handle, &write, Vec::new()))
                .await
                .unwrap_err();
            assert!(denied.contains("changes nothing on it"));
            assert_eq!(
                *store.0.lock().unwrap(),
                [(
                    workspace.route.app_id.clone(),
                    workspace.route.cluster_id.clone()
                )],
                "only the read reaches the store, under the admitted route"
            );

            let (connection, outgoing, handle) = workspace.connection(9, RouteAccess::Own);
            let mut endpoint = Endpoint::serve(connection, outgoing, handle);
            let denied = endpoint
                .call(ask(handle, &write, vec![1; 16]))
                .await
                .unwrap_err();
            assert!(denied.contains("no caller-scoped signer"));
            assert_eq!(store.0.lock().unwrap().len(), 1);
            endpoint
                .call(ask(handle, &write, Vec::new()))
                .await
                .unwrap();
            assert_eq!(store.0.lock().unwrap().len(), 2);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_service_command_is_answered_or_refused_on_its_call() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let workspace = Workspace::new(&[9]);
            let mut endpoint = workspace.endpoint(9);
            let channel = endpoint.channel;
            for service in [
                Service::AudioOutput,
                Service::NativeMedia,
                Service::Speaker,
                Service::Realtime,
                Service::NearbyTransfer,
                Service::WifiContinuity,
                Service::BrowserServiceSession,
                Service::ApprovedCheck,
                Service::LocalFile,
                Service::CommittedDocument,
                Service::RemoteAction,
                Service::Resources,
            ] {
                let refused = endpoint
                    .call(Operation::Command {
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
                    .call(Operation::Command {
                        channel: Handle(77),
                        service: Service::RemoteAction,
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
                        .call(Operation::Publish {
                            channel,
                            publication: Publication::Report(report.clone()),
                        })
                        .await
                        .is_err(),
                    "{report:?}: a channel with no instance or durable workspace reports nothing"
                );
            }
            assert!(
                endpoint
                    .call(Operation::Publish {
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
                Changes::GroupKeys,
                Changes::NearbyTransfers,
                Changes::Drives,
                Changes::Applications,
            ] {
                assert!(
                    endpoint
                        .call(Operation::Changed {
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
async fn an_app_takes_its_invocations_and_the_catalogue_and_answers_only_its_own() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let workspace = Workspace::new(&[13]);
            let mut endpoint = workspace.endpoint(13);
            let channel = endpoint.channel;
            assert_eq!(
                endpoint
                    .call(Operation::Subscribe {
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
                        .call(Operation::Invocation {
                            channel,
                            id: 404,
                            call: call.clone(),
                        })
                        .await
                        .is_err(),
                    "{call:?}: an invocation the app was never handed"
                );
            }
            assert!(
                endpoint
                    .call(Operation::Launch {
                        channel,
                        app_id: "so.tilo.notes".into(),
                        package_root: String::new(),
                        on_behalf_of: Vec::new(),
                    })
                    .await
                    .is_err(),
                "nothing the person or an invocation asked for is behind the launch"
            );
            assert_eq!(
                endpoint
                    .call(Operation::Subscribe {
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
async fn an_app_publishes_its_action_surface_as_a_change() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let workspace = Workspace::new(&[14]);
            let mut endpoint = workspace.endpoint(14);
            let channel = endpoint.channel;
            let reset = serde_json::to_string(&runtime_collab::SurfaceDelta {
                reset: true,
                ..runtime_collab::SurfaceDelta::default()
            })
            .unwrap();
            assert_eq!(
                endpoint
                    .call(Operation::Publish {
                        channel,
                        publication: Publication::Surface(reset),
                    })
                    .await,
                Ok(Reply::Done)
            );
            assert!(
                endpoint
                    .call(Operation::Publish {
                        channel,
                        publication: Publication::Surface("{}".into()),
                    })
                    .await
                    .is_err(),
                "a surface change that does not read is refused"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_wire_client_calls_and_subscribes_against_the_host() {
    use runtime_collab::wire::client::{Client, Delivery};
    tokio::task::LocalSet::new()
        .run_until(async {
            let workspace = Workspace::new(&[11, 12]);
            let connect = |seed: u8| {
                let (connection, outgoing, channel) = workspace.connection(seed, RouteAccess::Own);
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
                    .call(Operation::Publish {
                        channel: authored,
                        publication: Publication::Presence(PeerPresence {
                            key: postcard::to_stdvec(&key).unwrap(),
                            payload: PeerPresencePayload::Arrived,
                        }),
                    })
                    .await,
                Ok(Reply::Done)
            );
            let Some(Delivery::Event(Event::Presence { member, .. })) = presence.next().await
            else {
                panic!("the client routes the subscription's event");
            };
            assert_eq!(member, collab_types::MemberId::person(actor(12)));
            assert!(
                watcher
                    .call(Operation::Close { handle: Handle(99) })
                    .await
                    .is_err(),
                "a refusal reaches the call that caused it"
            );
            drop(presence);
        })
        .await;
}

/// One action of a record a window shows, which `workspace` holds when it is not the window's.
fn shown(workspace: Option<u128>) -> runtime_collab::shown_actions::ShownAction {
    runtime_collab::shown_actions::ShownAction::new(
        workspace
            .map(|id| crate::apps::test_support::WorkspaceId::from_u128(id).to_string())
            .as_deref(),
        "so.tilo.notes",
        ("test.playback", "person:test.playback", "mute"),
        &tilo_actions::ActionNeed::of(tilo_actions::ActionKind::Undoable),
    )
}

#[tokio::test(flavor = "current_thread")]
async fn a_shown_action_is_decided_by_the_workspace_holding_it_and_again_when_its_grants_change() {
    use runtime_collab::shown_actions::{ActionDecision, ActionVerdict};
    tokio::task::LocalSet::new()
        .run_until(async {
            let workspace = Workspace::new(&[21]);
            // The person only reads the workspace holding the record; the window's own
            // workspace, where they edit, does not decide it.
            let holding = crate::apps::test_support::WorkspaceId::from_u128(10);
            let mut held = crate::apps::test_support::Workspace::new(holding, "Their own");
            held.admit(actor(21), crate::apps::test_support::Role::Viewer);
            workspace.membership.write().workspaces.push(held);
            let mut endpoint = workspace.endpoint(21);
            let channel = endpoint.channel;
            let actions = vec![shown(Some(10)), shown(Some(11)), shown(None)];
            let subscription = endpoint
                .send(Operation::Subscribe {
                    channel,
                    topic: Topic::ShownActions(actions.clone()),
                })
                .await;
            assert_eq!(
                endpoint.frame().await,
                HostFrame::Reply {
                    id: subscription,
                    result: Ok(Reply::Subscribed),
                }
            );
            let decided = |verdicts: [ActionVerdict; 3]| HostFrame::Event {
                subscription,
                event: Event::ShownActions(
                    actions
                        .iter()
                        .cloned()
                        .zip(verdicts)
                        .map(|(action, verdict)| ActionDecision { action, verdict })
                        .collect(),
                ),
            };
            let not_held = ActionVerdict::Refused(super::shown_actions::NOT_HERE.into());
            assert_eq!(
                endpoint.frame().await,
                decided([
                    ActionVerdict::Refused(tilo_broker_core::shown::REFUSED.into()),
                    not_held.clone(),
                    ActionVerdict::Allowed,
                ]),
                "a reader of the holding workspace may not change the record, a workspace this \
                 host does not hold is refused in words the person reads, and the window's own \
                 workspace decides its own records"
            );

            workspace
                .membership
                .write()
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.id() == holding)
                .unwrap()
                .admit(actor(21), crate::apps::test_support::Role::Editor);
            workspace.state.refresh_policy_surfaces();
            assert_eq!(
                endpoint.frame().await,
                decided([ActionVerdict::Allowed, not_held, ActionVerdict::Allowed]),
                "the holding workspace's change reaches the window while it shows the record"
            );
        })
        .await;
}

/// What the device's own services publish (`device:server`), as the shell link reads it: a
/// reading and an action only for the Settings app, and a count of what asked it.
#[derive(Default)]
struct DeviceServer {
    readings: std::sync::atomic::AtomicUsize,
    actions: std::sync::atomic::AtomicUsize,
}

impl crate::apps::services::PublishedResources for DeviceServer {
    fn snapshot(
        &self,
        caller: &crate::apps::services::Caller,
        resource: &str,
    ) -> oneshot::Receiver<Result<crate::apps::services::Reading, String>> {
        let (reply, answered) = oneshot::channel();
        let answer = if resource != "device:server" {
            Ok((0, None))
        } else if caller.app_id == "so.tilo.settings" {
            self.readings.fetch_add(1, Ordering::Relaxed);
            Ok((1, Some(br#"{"work":{"from":"nobody"}}"#.to_vec())))
        } else {
            Err("this device's server settings are Settings' to show".to_owned())
        };
        let _ = reply.send(answer);
        answered
    }

    fn watch(
        &self,
        _: &crate::apps::services::Caller,
        _: &str,
        _: mpsc::UnboundedSender<runtime_collab::wire::Event>,
    ) -> Result<crate::apps::services::ResourceWatch, String> {
        Err("nothing watches the device's own resources here".to_owned())
    }

    fn unwatch(&self, _: crate::apps::services::ResourceWatch) {}

    fn invoke(
        &self,
        caller: &crate::apps::services::Caller,
        resource: &str,
        _: &str,
        _: Vec<u8>,
    ) -> oneshot::Receiver<runtime_collab::wire::Receipt> {
        let (reply, answered) = oneshot::channel();
        let receipt = if resource == "device:server" && caller.app_id == "so.tilo.settings" {
            self.actions.fetch_add(1, Ordering::Relaxed);
            runtime_collab::wire::Receipt::Done(Vec::new())
        } else {
            runtime_collab::wire::Receipt::Refused {
                code: "refused".to_owned(),
                message: "this device's server settings are Settings' to show".to_owned(),
            }
        };
        let _ = reply.send(receipt);
        answered
    }
}

/// An app's own connection names another app's id in a read and in an action: the device's own
/// services publish `device:server`, and answer it only for Settings there. The app's connection
/// is decided as the app it was admitted as, and the device's published resource is never asked.
#[tokio::test(flavor = "current_thread")]
async fn an_apps_own_connection_is_never_answered_as_another_app() {
    use tilo_actions::remote::{
        ActionAddress, InvokeArgument, InvokeRequest, RemoteActionFrame, RemoteActionMessage,
        ResourceReadRequest,
    };

    tokio::task::LocalSet::new()
        .run_until(async {
            let device = Arc::new(DeviceServer::default());
            let workspace = Workspace::serving(&[9], {
                let device = Arc::clone(&device);
                move |services| ProcessServices {
                    published: Some(
                        Arc::clone(&device) as Arc<dyn crate::apps::services::PublishedResources>
                    ),
                    ..services
                }
            });
            let mut endpoint = workspace.endpoint(9);
            let channel = endpoint.channel;

            let read = RemoteActionMessage::ResourceRead {
                correlation: 1,
                on_behalf_of: None,
                read: ResourceReadRequest {
                    workspace: workspace.route.cluster_id.clone(),
                    app: "so.tilo.settings".to_owned(),
                    record: "device:server".to_owned(),
                    joined: false,
                    after: None,
                    wait_ms: None,
                    document: false,
                    listing: false,
                    decide: false,
                },
            };
            let answered = endpoint
                .call(Operation::Command {
                    channel,
                    service: Service::RemoteAction,
                    body: RemoteActionFrame::new(read).encode().unwrap(),
                    on_behalf_of: Vec::new(),
                })
                .await;
            if let Ok(Reply::Answer(bytes)) = answered
                && let Ok(frame) = RemoteActionFrame::decode(&bytes)
                && let RemoteActionMessage::ResourceReading { result, .. } = frame.message
            {
                assert!(
                    result.is_err()
                        || result
                            .as_deref()
                            .is_none_or(|reading| !reading.contains("nobody")),
                    "an app's read names Settings, and is decided as the app: {result:?}"
                );
            }
            assert_eq!(
                device.readings.load(Ordering::Relaxed),
                0,
                "the device's published reading answers no app's connection on its own"
            );

            let invoke = RemoteActionMessage::Invoke(InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: "record:so.tilo.settings:device:server".to_owned(),
                    address: ActionAddress::new("device:server", "set-work"),
                },
                argument: InvokeArgument::Json(serde_json::Map::from_iter([(
                    "work".to_owned(),
                    serde_json::Value::from("anyone"),
                )])),
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            });
            let answered = endpoint
                .call(Operation::Command {
                    channel,
                    service: Service::RemoteAction,
                    body: RemoteActionFrame::new(invoke).encode().unwrap(),
                    on_behalf_of: Vec::new(),
                })
                .await;
            if let Ok(Reply::Answer(bytes)) = answered
                && let Ok(frame) = RemoteActionFrame::decode(&bytes)
                && let RemoteActionMessage::Receipt(receipt) = frame.message
            {
                assert!(
                    !matches!(receipt, runtime_collab::wire::Receipt::Done(_)),
                    "an app's action names Settings, and is decided as the app: {receipt:?}"
                );
            }
            assert_eq!(
                device.actions.load(Ordering::Relaxed),
                0,
                "the device's published action answers no app's connection on its own"
            );
        })
        .await;
}
