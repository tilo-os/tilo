//! Test-only collaboration host.
//!
//! This is deliberately a transport harness, not a second policy broker: it
//! speaks the collaboration wire, routes signed changes and presence to the
//! other subscribers on the same channel, and lets integration tests exercise
//! the real UI SDK shovel without a shell process or process-global
//! service-fd variables.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::os::fd::{IntoRawFd, RawFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use collab_types::PeerPresence;
use runtime_collab::OpAdmission;
use runtime_collab::wire::{
    Call, ContextUpdate, Event, FRAMING, Handle, HostFrame, Operation, Publication, Read, Reply,
    Service, Topic,
};
use tilo_ui::collaboration::CollaborationManager;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::mpsc;
use uuid::Uuid;

const TEST_APP_ID: &str = "ui-integration-test";

#[derive(Clone)]
struct Op {
    bytes: Vec<u8>,
    author: String,
}

#[derive(Clone)]
struct Presence {
    author: String,
    presence: PeerPresence,
}

struct Subscription<T> {
    peer: u64,
    tx: mpsc::UnboundedSender<T>,
}

#[derive(Default)]
struct Channel {
    ops: Vec<Subscription<Op>>,
    presence: Vec<Subscription<Presence>>,
    /// Directed snapshot requests, one stream per connection.
    snapshot_requests: Vec<Subscription<()>>,
}

#[derive(Default)]
struct Hub {
    next_peer: AtomicU64,
    channels: Mutex<HashMap<String, Channel>>,
}

impl Hub {
    /// Mirror the production broker: ask exactly one existing holder of the
    /// route (the lowest other connection) to publish a snapshot.
    fn request_snapshot(&self, channel: &str, requester: u64) {
        let mut channels = self.channels.lock().unwrap();
        let subscribers = &mut channels
            .entry(channel.to_owned())
            .or_default()
            .snapshot_requests;
        subscribers.retain(|sub| !sub.tx.is_closed());
        if let Some(holder) = subscribers
            .iter()
            .filter(|sub| sub.peer != requester)
            .min_by_key(|sub| sub.peer)
        {
            let _ = holder.tx.send(());
        }
        drop(channels);
    }

    fn fanout_op(&self, channel: &str, source: u64, value: &Op) {
        self.channels
            .lock()
            .unwrap()
            .entry(channel.to_owned())
            .or_default()
            .ops
            .retain(|sub| sub.peer == source || sub.tx.send(value.clone()).is_ok());
    }

    fn fanout_presence(&self, channel: &str, source: u64, value: &Presence) {
        self.channels
            .lock()
            .unwrap()
            .entry(channel.to_owned())
            .or_default()
            .presence
            .retain(|sub| sub.peer == source || sub.tx.send(value.clone()).is_ok());
    }
}

/// A shared typed fan-out service. Each call to [`connect`](Self::connect)
/// opens a fresh connection with explicit attribution.
#[derive(Clone, Default)]
pub struct TypedBroker {
    hub: Arc<Hub>,
}

impl TypedBroker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn connect(&self, user_id: &str) -> RawFd {
        self.connect_peer(user_id).1
    }

    fn connect_peer(&self, user_id: &str) -> (u64, RawFd) {
        let peer = self.hub.next_peer.fetch_add(1, Ordering::Relaxed);
        let connection = Connection {
            hub: self.hub.clone(),
            peer,
            user_id: user_id.to_owned(),
            channels: RefCell::default(),
            subscriptions: RefCell::default(),
            next_handle: Cell::new(1),
        };
        let (server, client) = std::os::unix::net::UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        client.set_nonblocking(true).unwrap();
        thread::Builder::new()
            .name(format!("ui-typed-collab-{peer}"))
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                tokio::task::LocalSet::new().block_on(&runtime, async move {
                    let stream = tokio::net::UnixStream::from_std(server).unwrap();
                    connection.serve(stream).await;
                });
            })
            .unwrap();
        (peer, client.into_raw_fd())
    }

    /// Connect a production `CollaborationManager` to the shared test room.
    ///
    /// Returns once the manager's route is admitted and this service holds
    /// its document and presence subscriptions, so traffic sent from then on
    /// reaches it without any settling time.
    pub fn manager(&self, user_id: &str) -> CollaborationManager {
        let (peer, fd) = self.connect_peer(user_id);
        let manager = CollaborationManager::for_route_fd(
            fd,
            TEST_APP_ID,
            Uuid::nil(),
            None,
            Some(user_id.to_owned()),
        )
        .expect("typed test endpoint connects");
        assert!(
            manager.is_connected(),
            "{user_id} must connect to the typed broker"
        );
        self.wait_subscribed(TEST_APP_ID, peer, user_id);
        manager
    }

    /// Wait until `peer` holds document and presence subscriptions on the
    /// `app_id` room.
    fn wait_subscribed(&self, app_id: &str, peer: u64, user_id: &str) {
        let channel =
            tilo_ui::collaboration::channel_key::format(app_id, &Uuid::nil().to_string(), "");
        tilo_test_env::wait_until(
            format!("{user_id}'s document and presence subscriptions on {channel}"),
            || {
                self.hub
                    .channels
                    .lock()
                    .unwrap()
                    .get(&channel)
                    .is_some_and(|room| {
                        room.ops.iter().any(|sub| sub.peer == peer)
                            && room.presence.iter().any(|sub| sub.peer == peer)
                    })
            },
        );
    }

    /// Connect a production `CollaborationManager` to its own `app_id` room,
    /// returning once this service holds its subscriptions there.
    pub fn route_manager(&self, user_id: &str, app_id: &str) -> CollaborationManager {
        let (peer, fd) = self.connect_peer(user_id);
        let manager = CollaborationManager::for_route_fd(
            fd,
            app_id,
            Uuid::nil(),
            None,
            Some(user_id.to_owned()),
        )
        .expect("typed test endpoint connects");
        self.wait_subscribed(app_id, peer, user_id);
        manager
    }
}

/// One endpoint's connection to the shared room.
struct Connection {
    hub: Arc<Hub>,
    peer: u64,
    user_id: String,
    /// The channel key each handle this connection opened names.
    channels: RefCell<HashMap<Handle, String>>,
    subscriptions: RefCell<HashMap<u64, tokio::task::AbortHandle>>,
    next_handle: Cell<u64>,
}

fn verify_routed_envelope(channel: &str, envelope: &collab_types::SignedOp) -> Result<(), String> {
    collab_types::verify_op(envelope).map_err(|error| format!("bad signature: {error:?}"))?;
    let (app_id, cluster_id, record_id) = tilo_ui::collaboration::channel_key::parse(channel);
    collab_types::validate_document_payload_route(&app_id, &cluster_id, &record_id, envelope)
        .map(|_| ())
        .map_err(|error| format!("envelope does not match its route: {error}"))
}

impl Connection {
    /// Answer the endpoint's calls on `stream` until it closes.
    #[expect(
        clippy::future_not_send,
        reason = "each connection is served on its own thread's `LocalSet`"
    )]
    async fn serve(self, stream: tokio::net::UnixStream) {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let (frames, mut outgoing) = mpsc::unbounded_channel::<HostFrame>();
        let write = async {
            while let Some(frame) = outgoing.recv().await {
                if FRAMING
                    .write_value_async(&mut writer, &frame)
                    .await
                    .is_err()
                    || writer.flush().await.is_err()
                {
                    break;
                }
            }
        };
        let read = async {
            while let Ok(Some(Call { id, request })) =
                FRAMING.read_value_async::<_, Call>(&mut reader).await
            {
                self.accept(id, request, &frames);
            }
        };
        tokio::select! {
            () = write => {}
            () = read => {}
        }
        for (_, task) in self.subscriptions.borrow_mut().drain() {
            task.abort();
        }
    }

    fn channel(&self, handle: Handle) -> Result<String, String> {
        self.channels
            .borrow()
            .get(&handle)
            .cloned()
            .ok_or_else(|| format!("no channel {handle:?} on this connection"))
    }

    fn accept(&self, id: u64, request: Operation, frames: &mpsc::UnboundedSender<HostFrame>) {
        let result = match request {
            Operation::Subscribe { channel, topic } => {
                match self
                    .channel(channel)
                    .and_then(|channel| self.subscribe(id, &channel, topic, frames))
                {
                    Ok(task) => {
                        self.subscriptions.borrow_mut().insert(id, task);
                        Ok(Reply::Subscribed)
                    }
                    Err(error) => Err(error),
                }
            }
            Operation::Unsubscribe { subscription } => {
                if let Some(task) = self.subscriptions.borrow_mut().remove(&subscription) {
                    task.abort();
                }
                return;
            }
            Operation::Close { handle } => {
                self.channels.borrow_mut().remove(&handle);
                return;
            }
            Operation::Answer { .. } => return,
            request => self.answer(request),
        };
        let _ = frames.send(HostFrame::Reply { id, result });
    }

    fn answer(&self, request: Operation) -> Result<Reply, String> {
        match request {
            Operation::Join(join) => {
                let handle = Handle(self.next_handle.get());
                self.next_handle.set(handle.0.wrapping_add(1));
                self.channels.borrow_mut().insert(handle, join.channel);
                Ok(Reply::Opened(handle))
            }
            Operation::Publish {
                channel,
                publication: Publication::Document(envelope),
            } => {
                let channel = self.channel(channel)?;
                // Route only what a production broker would accept: every
                // frame is an individually signed envelope bound to this
                // exact route.
                verify_routed_envelope(&channel, &envelope)?;
                self.hub.fanout_op(
                    &channel,
                    self.peer,
                    &Op {
                        bytes: postcard::to_stdvec(&*envelope).unwrap(),
                        author: self.user_id.clone(),
                    },
                );
                Ok(Reply::Done)
            }
            Operation::Publish {
                channel,
                publication: Publication::Presence(presence),
            } => {
                let channel = self.channel(channel)?;
                self.hub.fanout_presence(
                    &channel,
                    self.peer,
                    &Presence {
                        author: self.user_id.clone(),
                        presence,
                    },
                );
                Ok(Reply::Done)
            }
            Operation::Read {
                channel,
                read: Read::Document,
            } => {
                let channel = self.channel(channel)?;
                self.hub.request_snapshot(&channel, self.peer);
                Ok(Reply::Done)
            }
            // This harness stores no committed history: answer every open
            // with the explicit legacy mode so the route uses the
            // incremental op stream.
            Operation::Command {
                service: Service::CommittedDocument,
                ..
            } => runtime_collab::committed_document::Reply::Legacy
                .encode()
                .map(Reply::Answer)
                .map_err(Into::into),
            other => Err(format!("the typed test host does not answer {other:?}")),
        }
    }

    /// Start delivering `topic` of `channel` as the subscription `id`.
    fn subscribe(
        &self,
        id: u64,
        channel: &str,
        topic: Topic,
        frames: &mpsc::UnboundedSender<HostFrame>,
    ) -> Result<tokio::task::AbortHandle, String> {
        let frames = frames.clone();
        let deliver = move |event| {
            let _ = frames.send(HostFrame::Event {
                subscription: id,
                event,
            });
        };
        let mut channels = self.hub.channels.lock().unwrap();
        let room = channels.entry(channel.to_owned()).or_default();
        let task = match topic {
            Topic::Ops => {
                let (tx, mut rx) = mpsc::unbounded_channel();
                room.ops.push(Subscription {
                    peer: self.peer,
                    tx,
                });
                tokio::task::spawn_local(async move {
                    while let Some(op) = rx.recv().await {
                        deliver(Event::Op {
                            op: op.bytes,
                            from_user_id: op.author,
                            admission: OpAdmission::CleartextAdmitted,
                        });
                    }
                })
            }
            Topic::Presence => {
                let (tx, mut rx) = mpsc::unbounded_channel();
                room.presence.push(Subscription {
                    peer: self.peer,
                    tx,
                });
                tokio::task::spawn_local(async move {
                    while let Some(presence) = rx.recv().await {
                        deliver(Event::Presence {
                            user_id: presence.author,
                            presence: presence.presence,
                        });
                    }
                })
            }
            // Only directed snapshot requests: this harness has no roster or
            // permission snapshots to publish.
            Topic::Context => {
                let (tx, mut rx) = mpsc::unbounded_channel();
                room.snapshot_requests.push(Subscription {
                    peer: self.peer,
                    tx,
                });
                tokio::task::spawn_local(async move {
                    while rx.recv().await.is_some() {
                        deliver(Event::Context(ContextUpdate::SnapshotRequested));
                    }
                })
            }
            Topic::Catalogue => tokio::task::spawn_local(async move {
                deliver(Event::Catalogue(
                    serde_json::json!({"version": 1, "surfaces": []}).to_string(),
                ));
                std::future::pending::<()>().await;
            }),
            other => return Err(format!("the typed test host serves no {other:?} topic")),
        };
        drop(channels);
        Ok(task.abort_handle())
    }
}
