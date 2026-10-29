#![expect(
    clippy::future_not_send,
    reason = "wire connections and the broker registry run on the test LocalSet"
)]

use super::*;
use crate::collab_outbound::recording::{RecordingSink, Sent};
use collab_types::{PeerPresence, PeerPresencePayload, sign_op};
use ed25519_dalek::SigningKey;
use std::time::SystemTime;

struct Room {
    state: Arc<CapnpBrokerState>,
    route: CrdtRoute,
    sink: Arc<RecordingSink>,
    membership: crate::test_support::WorkspaceTreeHandle,
    directories: Rc<LocalActionDirectoryRegistry>,
    _fanout: Arc<FanoutState>,
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
            None,
        );
        fanout.attach_capnp(state.clone());
        state.ensure_channel(&route.channel_key());
        Self {
            state,
            route,
            sink,
            membership,
            directories: Rc::new(LocalActionDirectoryRegistry::new(bridge_rx)),
            _fanout: fanout,
        }
    }

    /// An endpoint of `seed`'s person on its own wire connection, holding
    /// the room's channel.
    fn endpoint(&self, seed: u8) -> Endpoint {
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
        let (connection, outgoing) = WireConnection::new();
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
        let (host, endpoint) = tokio::net::UnixStream::pair().unwrap();
        let (reader, writer) = host.into_split();
        let driver = tokio::task::spawn_local(serve(connection, outgoing, reader, writer));
        Endpoint {
            channel,
            stream: endpoint,
            next_id: 1,
            driver,
        }
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
    async fn send(&mut self, request: Request) -> u64 {
        use tokio::io::AsyncWriteExt as _;
        let id = self.next_id;
        self.next_id += 1;
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
