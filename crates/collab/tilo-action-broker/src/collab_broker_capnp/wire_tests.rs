#![expect(
    clippy::future_not_send,
    reason = "wire connections and the broker registry run on the test LocalSet"
)]

use super::*;
use crate::collab_outbound::recording::{RecordingSink, Sent};
use collab_types::{PeerPresence, PeerPresencePayload};
use ed25519_dalek::SigningKey;

struct Room {
    state: Arc<CapnpBrokerState>,
    route: CrdtRoute,
    sink: Arc<RecordingSink>,
    membership: crate::test_support::WorkspaceTreeHandle,
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
        let route = CrdtRoute::cluster(workspace.room().to_string());
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let membership = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let sink = Arc::new(RecordingSink::default());
        let (state, _) = CapnpBrokerState::new(
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
                device_public_key: [seed.wrapping_add(100); 32],
                capped: false,
            },
        )
        .unwrap();
        let (connection, outgoing) = WireConnection::new();
        let channel = connection.open(ChannelSession {
            presence: PresenceSession::new(
                self.state.clone(),
                self.route.channel_key(),
                self.state.next_peer_id(),
                admitted,
            ),
        });
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
