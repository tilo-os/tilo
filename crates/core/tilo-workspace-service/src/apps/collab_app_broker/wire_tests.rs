#![allow(
    clippy::significant_drop_tightening,
    reason = "each test's workspace outlives the endpoints it serves; dropping it early would close them"
)]

use super::*;
use crate::apps::test_support::WorkspaceTree;
use collab_types::presence::{Payload, Presence};
use ed25519_dalek::SigningKey;
use std::sync::atomic::Ordering;
use tokio::sync::oneshot;

struct Workspace {
    state: Arc<AppBrokerState>,
    route: CrdtRoute,
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
            workspace
                .policy
                .add_member(actor(*seed), crate::apps::test_support::Role::Editor);
        }
        let route = CrdtRoute::record("so.tilo.notes", workspace.id().to_string(), "meeting-notes");
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let membership = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let (state, bridge_rx) = AppBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
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
            membership,
            directories: Rc::new(LocalActionDirectoryRegistry::new(bridge_rx)),
            fanout,
        }
    }

    /// An endpoint of `seed`'s person on its own wire connection, holding
    /// the workspace's route.
    fn endpoint(&self, seed: u8) -> Endpoint {
        let (connection, outgoing, channel) = self.connection(seed);
        Endpoint::serve(connection, outgoing, channel)
    }

    /// A connection of `seed`'s person holding the workspace's route, not yet
    /// served.
    fn connection(
        &self,
        seed: u8,
    ) -> (
        Rc<WireConnection>,
        mpsc::UnboundedReceiver<HostFrame>,
        Record,
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
        let key = connection.open(ChannelSession::new(Rc::new(JoinedChannel {
            committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
            state: self.state.clone(),
            directories: self.directories.clone(),
            channel_id: self.route.channel_key(),
            peer_id: hold,
            connection_id: hold,
            verified_identity: admitted,
            resource_type: None,
            lease: None,
            _hold: None,
        })));
        (connection, outgoing, Record::new(key, ""))
    }

    /// A connection of an app acting as `seed`'s person on `device`, served to an endpoint
    /// naming the workspace's record, which the connection opens on its first call about it.
    fn opening_endpoint(&self, seed: u8, device: &SigningKey) -> Endpoint {
        let peer_id = self.fanout.next_peer_id();
        let service = Rc::new(AppConnection {
            worker: None,
            app_worker: None,
            identity: Some(session::ConnectionIdentity::certified(
                crate::apps::collab_app_broker::tests::device_certificate(
                    &SigningKey::from_bytes(&[seed; 32]),
                    device,
                ),
                None,
            )),
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
        Endpoint::serve(
            connection,
            outgoing,
            Record::new(self.route.channel_key(), ""),
        )
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
    channel: Record,
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
        channel: Record,
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

    /// Say `payload` under `key` as the app of the endpoint's record, in its workspace.
    async fn say(&mut self, key: &PresenceKey, payload: Payload) -> Result<Reply, String> {
        let record = self.channel.clone();
        let route = CrdtRoute::from_channel_key(&record.key);
        let frame = Presence::app(route.app_id, route.cluster_id, key.clone(), payload);
        self.call(Operation::Say { record, frame }).await
    }

    /// The next word someone else said, on the subscription `subscription`, past the context
    /// it starts with.
    async fn heard(&mut self, subscription: u64) -> (collab_types::MemberId, Presence) {
        loop {
            match self.frame().await {
                HostFrame::Event {
                    subscription: delivered,
                    event: Event::Context(ContextUpdate::Heard { member, frame }),
                } if delivered == subscription => return (member, *frame),
                HostFrame::Event {
                    event: Event::Context(_),
                    ..
                } => {}
                other => panic!("expected a word on {subscription}, got {other:?}"),
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn presence_crosses_the_wire_attributed_and_replicated() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let workspace = Workspace::new(&[1, 2]);
            let mut watcher = workspace.endpoint(1);
            let mut author = workspace.endpoint(2);
            let channel = watcher.channel.clone();
            let subscription = watcher
                .send(Operation::Subscribe {
                    record: channel.clone(),
                    topic: Topic::Context,
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
                    .say(
                        &key,
                        Payload::Status {
                            text: "  Reviewing   Q2 ".into(),
                            automatic: false,
                        },
                    )
                    .await,
                Ok(Reply::Done)
            );
            let (member, presence) = watcher.heard(subscription).await;
            assert_eq!(
                member,
                collab_types::MemberId::person(actor(2)),
                "attributed"
            );
            assert_eq!(presence.key, key);
            assert_eq!(
                presence.payload,
                Payload::Status {
                    text: "Reviewing Q2".into(),
                    automatic: false,
                },
                "the broker trims a status before it travels"
            );

            assert!(
                author
                    .say(
                        &key,
                        Payload::Availability {
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
            author.say(&key, Payload::Arrived).await.unwrap();
            let channel = watcher.channel.clone();
            assert_eq!(
                watcher
                    .call(Operation::Leave {
                        record: channel.key
                    })
                    .await,
                Ok(Reply::Done),
                "nothing more arrives on an ended subscription before the next reply"
            );
            assert!(
                watcher.say(&key, Payload::Left).await.is_err(),
                "a record the connection left names nothing on it"
            );
        })
        .await;
}

/// The device's copy of the one record the test's route opened, as its workspace service keeps
/// it: an app's changes go in, each change whole, and a window reads the changes past what it
/// holds.
#[derive(Default)]
struct KeptRecord {
    record: Mutex<automerge::Automerge>,
    writes: std::sync::atomic::AtomicUsize,
}

impl crate::apps::services::ResourceServices for KeptRecord {
    fn command(
        &self,
        context: &crate::apps::services::ResourceRequestContext,
        command: tilo_resource_store::wire::Command,
    ) -> crate::apps::services::ResourceAnswer {
        use tilo_resource_store::wire::{Bytes, Command, Reply};
        assert_eq!(
            context.record(),
            Some("meeting-notes"),
            "the route's record"
        );
        let mut record = self.record.lock().unwrap();
        let answer = match command {
            Command::RecordChange {
                changes,
                more: false,
                commit: None,
                record: None,
            } => {
                self.writes.fetch_add(1, Ordering::Relaxed);
                record
                    .load_incremental(&changes.0)
                    .map(|_| Reply::Written)
                    .map_err(|error| error.to_string())
            }
            Command::RecordChanges {
                after,
                from: 0,
                record: None,
            } => {
                let after: Vec<automerge::ChangeHash> =
                    after.iter().map(|head| head.parse().unwrap()).collect();
                Ok(Reply::RecordChanges {
                    changes: Bytes(
                        record
                            .get_changes(&after)
                            .iter()
                            .flat_map(|change| change.raw_bytes().iter().copied())
                            .collect(),
                    ),
                    heads: record.get_heads().iter().map(ToString::to_string).collect(),
                    next: None,
                    pending: false,
                })
            }
            other => Err(format!("the record answers no {other:?}")),
        };
        drop(record);
        Box::pin(std::future::ready(answer))
    }

    fn open(
        &self,
        _: &crate::apps::services::ResourceRequestContext,
        _: tilo_resource_store::Query,
    ) -> crate::apps::services::ResourcePages {
        Box::pin(std::future::ready(Err("the record holds no query".into())))
    }
}

/// The device's one reader as the test serves it: every read and change of a record allowed
/// while `allows` says so, and refused after.
struct Decides(AtomicBool);

impl Decides {
    fn decided<'a>(&self, refusal: &str) -> tilo_actions::remote::ReadsAnswer<'a, ()> {
        Box::pin(std::future::ready(if self.0.load(Ordering::Relaxed) {
            Ok(())
        } else {
            Err(tilo_actions::remote::ActionCallError::Refused {
                reason: refusal.into(),
            })
        }))
    }
}

impl tilo_actions::remote::ResourceReads for Decides {
    fn may_read<'a>(
        &'a self,
        _: &'a tilo_actions::remote::Caller,
        _: &'a tilo_actions::remote::ResourceAddress,
    ) -> tilo_actions::remote::ReadsAnswer<'a, ()> {
        self.decided(tilo_actions::remote::RESOURCE_READ_REFUSED)
    }

    fn may_write<'a>(
        &'a self,
        _: &'a tilo_actions::remote::Caller,
        _: &'a tilo_actions::remote::ResourceAddress,
    ) -> tilo_actions::remote::ReadsAnswer<'a, ()> {
        self.decided(tilo_actions::remote::RESOURCE_WRITE_REFUSED)
    }

    fn answer_read(
        self: Arc<Self>,
        _: &tilo_actions::remote::Caller,
        correlation: u64,
        _: tilo_actions::remote::ResourceReadRequest,
    ) -> tilo_actions::remote::ReadingAnswer {
        Box::pin(std::future::ready(
            tilo_actions::remote::RemoteActionMessage::ResourceReading {
                correlation,
                result: Err(tilo_actions::remote::ActionCallError::Unavailable {
                    reason: "the test decides alone".into(),
                }),
            },
        ))
    }
}

impl Workspace {
    /// A workspace like [`Self::new`]'s whose route's record the device keeps, read and changed
    /// as `decides` lets each caller.
    fn keeping_the_record(members: &[u8]) -> (Self, Arc<Decides>, Arc<KeptRecord>) {
        let kept = Arc::new(KeptRecord::default());
        let resources = Arc::clone(&kept) as Arc<dyn crate::apps::services::ResourceServices>;
        let workspace = Self::serving(members, |services| ProcessServices {
            resources: Some(resources),
            ..services
        });
        let decides = Arc::new(Decides(AtomicBool::new(true)));
        workspace
            .state
            .serve_records(Arc::clone(&decides) as Arc<dyn tilo_actions::remote::ResourceReads>);
        (workspace, decides, kept)
    }
}

impl Endpoint {
    /// Ask the device's one store `command` on this endpoint's channel.
    async fn record(
        &mut self,
        command: &tilo_resource_store::wire::Command,
    ) -> Result<tilo_resource_store::wire::Reply, String> {
        let channel = self.channel.clone();
        let reply = self
            .call(Operation::Command {
                record: channel.clone(),
                service: Service::Resources,
                body: serde_json::to_vec(command).unwrap(),
                on_behalf_of: Vec::new(),
            })
            .await?;
        let Reply::Answer(answer) = reply else {
            panic!("a store command is answered, not {reply:?}");
        };
        Ok(serde_json::from_slice(&answer).unwrap())
    }

    /// Subscribe this endpoint to its channel's context.
    async fn follow_context(&mut self) -> u64 {
        let channel = self.channel.clone();
        let subscription = self
            .send(Operation::Subscribe {
                record: channel.clone(),
                topic: Topic::Context,
            })
            .await;
        assert_eq!(
            self.frame().await,
            HostFrame::Reply {
                id: subscription,
                result: Ok(Reply::Subscribed),
            }
        );
        subscription
    }
}

/// An app's change of a record, as Automerge encodes it: its title set to `title`, on top of
/// `on`.
fn titled(on: &mut automerge::AutoCommit, title: &str) -> Vec<u8> {
    use automerge::transaction::Transactable as _;
    let heads = on.get_heads();
    on.put(automerge::ROOT, "title", title).unwrap();
    on.save_after(&heads)
}

/// A change of the route's record of `changes`, whole.
fn change(changes: Vec<u8>) -> tilo_resource_store::wire::Command {
    tilo_resource_store::wire::Command::RecordChange {
        changes: tilo_resource_store::wire::Bytes(changes),
        more: false,
        commit: None,
        record: None,
    }
}

/// A read of the route's record's changes past `after`, from its first page.
fn changes_after(after: Vec<String>) -> tilo_resource_store::wire::Command {
    tilo_resource_store::wire::Command::RecordChanges {
        after,
        from: 0,
        record: None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_change_crosses_the_wire_to_the_other_windows_on_the_record() {
    use tilo_resource_store::wire::Reply as Store;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (workspace, _, _) = Workspace::keeping_the_record(&[4, 5]);
            let mut watcher = workspace.endpoint(4);
            let mut author = workspace.endpoint(5);
            let subscription = watcher.follow_context().await;
            author.follow_context().await;

            let mut note = automerge::AutoCommit::new();
            let change = titled(&mut note, "Standup");
            assert!(matches!(
                author.record(&self::change(change.clone())).await,
                Ok(Store::Written)
            ));
            assert_eq!(
                watcher.frame().await,
                HostFrame::Event {
                    subscription,
                    event: Event::Context(ContextUpdate::RecordChanged),
                },
                "the watcher is told the record changed"
            );
            let Ok(Store::RecordChanges {
                changes,
                heads,
                next: None,
                ..
            }) = watcher.record(&changes_after(Vec::new())).await
            else {
                panic!("the watcher reads the record's changes");
            };
            assert_eq!(changes.0, change, "the change the author made");
            assert_eq!(
                heads,
                note.get_heads()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            );
            // The author is not told of its own change: the next frame it reads answers its next
            // call.
            assert!(matches!(
                author.record(&changes_after(heads)).await,
                Ok(Store::RecordChanges { changes, .. }) if changes.0.is_empty()
            ));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_late_joiner_reads_the_record_from_the_device_as_its_reader_decides() {
    use tilo_resource_store::wire::Reply as Store;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (workspace, decides, _) = Workspace::keeping_the_record(&[6, 8]);
            let mut holder = workspace.endpoint(6);
            holder.follow_context().await;
            let mut note = automerge::AutoCommit::new();
            let first = titled(&mut note, "Groceries");
            let second = titled(&mut note, "Groceries, oat milk");
            let both = [first, second].concat();
            assert!(matches!(
                holder.record(&change(both.clone())).await,
                Ok(Store::Written)
            ));

            let mut joiner = workspace.endpoint(8);
            let Ok(Store::RecordChanges { changes, heads, .. }) =
                joiner.record(&changes_after(Vec::new())).await
            else {
                panic!("the joiner reads the record from the device");
            };
            assert_eq!(changes.0, both, "every change, in order");
            assert!(
                matches!(
                    joiner.record(&changes_after(heads.clone())).await,
                    Ok(Store::RecordChanges {
                        changes,
                        heads: now,
                        next: None,
                        ..
                    })
                        if changes.0.is_empty() && now == heads
                ),
                "nothing past what it holds"
            );

            decides.0.store(false, Ordering::Relaxed);
            let refused = joiner.record(&changes_after(Vec::new())).await.unwrap_err();
            assert!(
                refused.contains(tilo_actions::remote::RESOURCE_READ_REFUSED),
                "{refused}"
            );
            let refused = joiner
                .record(&change(titled(&mut note, "Mine now")))
                .await
                .unwrap_err();
            assert!(
                refused.contains(tilo_actions::remote::RESOURCE_WRITE_REFUSED),
                "{refused}"
            );
            // No other window was asked for the record, and the refused change told nobody: the
            // holder's next frame answers its own next call.
            assert!(holder.record(&changes_after(Vec::new())).await.is_err());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_change_sent_in_runs_lands_as_one_write() {
    use tilo_resource_store::wire::{Bytes, Command, Reply as Store};
    tokio::task::LocalSet::new()
        .run_until(async {
            let (workspace, _, kept) = Workspace::keeping_the_record(&[4, 5]);
            let mut watcher = workspace.endpoint(4);
            let mut author = workspace.endpoint(5);
            let subscription = watcher.follow_context().await;
            author.follow_context().await;
            let mut note = automerge::AutoCommit::new();
            let whole = [
                titled(&mut note, "Plan"),
                titled(&mut note, "Plan, revised"),
            ]
            .concat();

            // Runs cut anywhere, even inside one change, are kept until the last.
            let cuts = [whole.len() / 3, whole.len() * 2 / 3];
            for run in [&whole[..cuts[0]], &whole[cuts[0]..cuts[1]]] {
                assert!(matches!(
                    author
                        .record(&Command::RecordChange {
                            changes: Bytes(run.to_vec()),
                            more: true,
                            commit: None,
                            record: None,
                        })
                        .await,
                    Ok(Store::Written)
                ));
            }
            assert_eq!(
                kept.writes.load(Ordering::Relaxed),
                0,
                "no run is written alone"
            );
            assert!(matches!(
                author.record(&change(whole[cuts[1]..].to_vec())).await,
                Ok(Store::Written)
            ));
            assert_eq!(
                kept.writes.load(Ordering::Relaxed),
                1,
                "the runs are one write"
            );
            assert_eq!(
                watcher.frame().await,
                HostFrame::Event {
                    subscription,
                    event: Event::Context(ContextUpdate::RecordChanged),
                },
                "the watcher is told once, of the whole change"
            );
            assert!(matches!(
                watcher.record(&changes_after(Vec::new())).await,
                Ok(Store::RecordChanges { changes, .. }) if changes.0 == whole
            ));
        })
        .await;
}

/// An app's connection opens a record on its first call naming it, as the person its launch
/// admitted it as: a device the workspace never enrolled opens nothing, the member's own device
/// works in it.
#[tokio::test(flavor = "current_thread")]
async fn a_record_opens_on_the_first_call_naming_it() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let workspace = Workspace::new(&[3]);
            let mut endpoint = workspace.opening_endpoint(3, &device(4));
            assert_eq!(
                endpoint
                    .call(Operation::Workspace {
                        record: workspace.route.channel_key(),
                        seen: None,
                    })
                    .await,
                Ok(Reply::Workspace(runtime_collab::WorkspaceStatus::Ready))
            );
            let refused = endpoint
                .say(&PresenceKey::surface_pointer("editor"), Payload::Arrived)
                .await;
            assert!(
                refused.is_err(),
                "a device the workspace never enrolled opens nothing"
            );
            let mut endpoint = workspace.opening_endpoint(3, &device(3));
            assert_eq!(
                endpoint
                    .say(&PresenceKey::surface_pointer("editor"), Payload::Arrived)
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
            let channel = app.channel.clone();
            let query =
                serde_json::to_vec(&tilo_resource_store::Query::of("tasks.issue", 10)).unwrap();
            let subscription = app
                .send(Operation::Subscribe {
                    record: channel.clone(),
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
async fn an_app_reads_through_its_certified_route_and_its_writes_are_decided_by_the_device_reader()
{
    use tilo_resource_store::wire::Command;

    tokio::task::LocalSet::new()
        .run_until(async {
            let store = Arc::new(CountingStore::default());
            let served = Arc::clone(&store);
            let workspace = Workspace::serving(&[9], move |services| ProcessServices {
                resources: Some(served),
                ..services
            });
            let decides = Arc::new(Decides(AtomicBool::new(false)));
            let reads: Arc<dyn tilo_actions::remote::ResourceReads> = decides.clone();
            workspace.state.serve_records(reads);
            let (connection, outgoing, handle) = workspace.connection(9);
            let mut endpoint = Endpoint::serve(connection, outgoing, handle.clone());
            let ask = |handle, command: &Command, on_behalf_of| Operation::Command {
                record: handle,
                service: Service::Resources,
                body: serde_json::to_vec(command).unwrap(),
                on_behalf_of,
            };

            let answer = endpoint
                .call(ask(
                    handle.clone(),
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
                .call(ask(handle.clone(), &write, Vec::new()))
                .await
                .unwrap_err();
            assert!(denied.contains("may not change the record"), "{denied}");
            assert!(denied.contains(workspace.route.app_id.as_str()), "{denied}");
            assert_eq!(
                *store.0.lock().unwrap(),
                [(
                    workspace.route.app_id.clone(),
                    workspace.route.cluster_id.clone()
                )],
                "only the read reaches the store, under the admitted route"
            );

            // The reader allows the write: it reaches the store, whatever the route's own
            // access says, which decides no resource write.
            decides.0.store(true, Ordering::Relaxed);
            endpoint
                .call(ask(handle.clone(), &write, Vec::new()))
                .await
                .unwrap();
            assert_eq!(store.0.lock().unwrap().len(), 2);

            let (connection, outgoing, handle) = workspace.connection(9);
            let mut endpoint = Endpoint::serve(connection, outgoing, handle.clone());
            let denied = endpoint
                .call(ask(handle.clone(), &write, vec![1; 16]))
                .await
                .unwrap_err();
            assert!(denied.contains("no caller-scoped signer"));
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
            let channel = endpoint.channel.clone();
            for service in [
                Service::AudioOutput,
                Service::NativeMedia,
                Service::BrowserServiceSession,
                Service::LocalFile,
                Service::CommittedDocument,
                Service::RemoteAction,
                Service::Resources,
            ] {
                let refused = endpoint
                    .call(Operation::Command {
                        record: channel.clone(),
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
                        record: Record::new("so.tilo.notes/elsewhere", ""),
                        service: Service::RemoteAction,
                        body: Vec::new(),
                        on_behalf_of: Vec::new(),
                    })
                    .await
                    .is_err(),
                "a command names a record the connection holds"
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
                            record: channel.clone(),
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
                        record: channel.clone(),
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
            for of in [Changes::GroupKeys] {
                assert!(
                    endpoint
                        .call(Operation::Changed {
                            record: channel.clone(),
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
async fn an_app_takes_its_invocations_and_answers_only_its_own() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let workspace = Workspace::new(&[13]);
            let mut endpoint = workspace.endpoint(13);
            let channel = endpoint.channel.clone();
            assert_eq!(
                endpoint
                    .call(Operation::Subscribe {
                        record: channel.clone(),
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
                            record: channel.clone(),
                            id: 404,
                            call: call.clone(),
                        })
                        .await
                        .is_err(),
                    "{call:?}: an invocation the app was never handed"
                );
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_app_publishes_its_action_surface_as_a_change() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let workspace = Workspace::new(&[14]);
            let mut endpoint = workspace.endpoint(14);
            let channel = endpoint.channel.clone();
            let reset = serde_json::to_string(&runtime_collab::SurfaceDelta {
                reset: true,
                ..runtime_collab::SurfaceDelta::default()
            })
            .unwrap();
            assert_eq!(
                endpoint
                    .call(Operation::Publish {
                        record: channel.clone(),
                        publication: Publication::Surface(reset),
                    })
                    .await,
                Ok(Reply::Done)
            );
            assert!(
                endpoint
                    .call(Operation::Publish {
                        record: channel.clone(),
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
                let (connection, outgoing, channel) = workspace.connection(seed);
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
                .subscribe(watcher_channel, Topic::Context)
                .await
                .unwrap();
            let key = PresenceKey::surface_pointer("editor");
            let route = CrdtRoute::from_channel_key(&authored.key);
            let frame = Presence::app(route.app_id, route.cluster_id, key, Payload::Arrived);
            assert_eq!(
                author
                    .call(Operation::Say {
                        record: authored,
                        frame,
                    })
                    .await,
                Ok(Reply::Done)
            );
            let member = loop {
                match presence.next().await {
                    Some(Delivery::Event(Event::Context(ContextUpdate::Heard {
                        member, ..
                    }))) => {
                        break member;
                    }
                    Some(Delivery::Event(Event::Context(_))) => {}
                    _ => panic!("the client routes the subscription's event"),
                }
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
            held.policy
                .add_member(actor(21), crate::apps::test_support::Role::Viewer);
            workspace.membership.write().workspaces.push(held);
            let mut endpoint = workspace.endpoint(21);
            let channel = endpoint.channel.clone();
            let actions = vec![shown(Some(10)), shown(Some(11)), shown(None)];
            let subscription = endpoint
                .send(Operation::Subscribe {
                    record: channel.clone(),
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
                .policy
                .add_member(actor(21), crate::apps::test_support::Role::Editor);
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
    use tilo_actions::api::ActionAddress;
    use tilo_actions::remote::{
        InvokeArgument, InvokeRequest, RemoteActionFrame, RemoteActionMessage, ResourceReadRequest,
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
            let channel = endpoint.channel.clone();

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
                    answer: tilo_actions::remote::ReadAnswer::Reading,
                },
            };
            let answered = endpoint
                .call(Operation::Command {
                    record: channel.clone(),
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
                    !result
                        .as_deref()
                        .is_ok_and(|reading| reading.contains("nobody")),
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
                    record: channel.clone(),
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
                    !matches!(
                        receipt.status,
                        tilo_actions::remote::ReceiptStatus::Completed { .. }
                    ),
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
