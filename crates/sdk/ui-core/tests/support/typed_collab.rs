//! Test-only Cap'n Proto collaboration service.
//!
//! This is deliberately a transport harness, not a second policy broker: it
//! implements the runtime schema, routes opaque payloads to other subscribers
//! on the same channel, and lets integration tests exercise the real UI SDK
//! shovel without a shell process or process-global service-fd variables.

#![allow(refining_impl_trait)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::fd::{IntoRawFd, RawFd};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use capnp::capability::Promise;
use runtime_collab::raw::{
    action_catalogue_stream, action_stream, channel_handle, collab_service, context_stream,
    presence_event, presence_handle, presence_payload, presence_stream, stream,
};
use tilo_ui::collaboration::CollaborationManager;
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
    key: Vec<u8>,
    payload: PresenceValue,
}

#[derive(Clone)]
enum PresenceValue {
    Pointer(f32, f32),
    Caret(u64),
    Selection(u64, u64),
    Arrived,
    Left,
}

#[derive(Clone)]
struct Action {
    type_id: String,
    payload: Vec<u8>,
    author: String,
}

struct Subscription<T> {
    peer: u64,
    tx: mpsc::UnboundedSender<T>,
}

#[derive(Default)]
struct Channel {
    ops: Vec<Subscription<Op>>,
    presence: Vec<Subscription<Presence>>,
    actions: Vec<Subscription<Action>>,
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

    fn fanout_action(&self, channel: &str, source: u64, value: &Action) {
        self.channels
            .lock()
            .unwrap()
            .entry(channel.to_owned())
            .or_default()
            .actions
            .retain(|sub| sub.peer == source || sub.tx.send(value.clone()).is_ok());
    }
}

/// A shared typed fan-out service. Each call to [`connect`](Self::connect)
/// creates a fresh RPC session with explicit attribution.
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
        let service = Service {
            hub: self.hub.clone(),
            peer,
            user_id: user_id.to_owned(),
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
                    let bootstrap: collab_service::Client = capnp_rpc::new_client(service);
                    let _ = runtime_portal::serve_connected_io(stream, bootstrap.client).await;
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

    /// Publish an action as an external room participant. This drives the
    /// SDK action subscription without adding a toolkit-only request variant.
    pub fn inject_action(&self, author: &str, type_id: &str, payload: &[u8]) {
        let channel =
            tilo_ui::collaboration::channel_key::format(TEST_APP_ID, &Uuid::nil().to_string(), "");
        self.hub.fanout_action(
            &channel,
            u64::MAX,
            &Action {
                type_id: type_id.to_owned(),
                payload: payload.to_vec(),
                author: author.to_owned(),
            },
        );
    }
}

struct Service {
    hub: Arc<Hub>,
    peer: u64,
    user_id: String,
}

impl collab_service::Server for Service {
    fn join_channel(
        self: Rc<Self>,
        params: collab_service::JoinChannelParams,
        mut results: collab_service::JoinChannelResults,
    ) -> Promise<(), capnp::Error> {
        let channel = match params
            .get()
            .and_then(collab_service::join_channel_params::Reader::get_channel_id)
            .and_then(|s| s.to_str().map_err(|e| capnp::Error::failed(e.to_string())))
        {
            Ok(value) => value.to_owned(),
            Err(error) => return Promise::err(error),
        };
        let client: channel_handle::Client = capnp_rpc::new_client(ChannelHandle {
            hub: self.hub.clone(),
            peer: self.peer,
            user_id: self.user_id.clone(),
            channel,
        });
        results.get().set_handle(client);
        Promise::ok(())
    }
}

fn verify_routed_envelope(channel: &str, bytes: &[u8]) -> Result<(), String> {
    let envelope: collab_types::SignedOp =
        postcard::from_bytes(bytes).map_err(|error| format!("not a SignedOp: {error}"))?;
    collab_types::verify_op(&envelope).map_err(|error| format!("bad signature: {error:?}"))?;
    let (app_id, cluster_id, record_id) = tilo_ui::collaboration::channel_key::parse(channel);
    collab_types::validate_document_payload_route(&app_id, &cluster_id, &record_id, &envelope)
        .map(|_| ())
        .map_err(|error| format!("envelope does not match its route: {error}"))
}

struct ChannelHandle {
    hub: Arc<Hub>,
    peer: u64,
    user_id: String,
    channel: String,
}

impl channel_handle::Server for ChannelHandle {
    fn send(
        self: Rc<Self>,
        params: channel_handle::SendParams,
        _results: channel_handle::SendResults,
    ) -> Promise<(), capnp::Error> {
        let bytes = match params
            .get()
            .and_then(channel_handle::send_params::Reader::get_op)
        {
            Ok(value) => value.to_vec(),
            Err(error) => return Promise::err(error),
        };
        // Route only what a production broker would accept: every frame is
        // an individually signed envelope bound to this exact route.
        if let Err(error) = verify_routed_envelope(&self.channel, &bytes) {
            return Promise::err(capnp::Error::failed(error));
        }
        self.hub.fanout_op(
            &self.channel,
            self.peer,
            &Op {
                bytes,
                author: self.user_id.clone(),
            },
        );
        Promise::ok(())
    }

    fn subscribe(
        self: Rc<Self>,
        _params: channel_handle::SubscribeParams,
        mut results: channel_handle::SubscribeResults,
    ) -> Promise<(), capnp::Error> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.hub
            .channels
            .lock()
            .unwrap()
            .entry(self.channel.clone())
            .or_default()
            .ops
            .push(Subscription {
                peer: self.peer,
                tx,
            });
        let client: stream::Client = capnp_rpc::new_client(OpStream {
            rx: Rc::new(RefCell::new(rx)),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn presence(
        self: Rc<Self>,
        _params: channel_handle::PresenceParams,
        mut results: channel_handle::PresenceResults,
    ) -> Promise<(), capnp::Error> {
        let client: presence_handle::Client = capnp_rpc::new_client(PresenceHandle {
            hub: self.hub.clone(),
            peer: self.peer,
            user_id: self.user_id.clone(),
            channel: self.channel.clone(),
        });
        results.get().set_handle(client);
        Promise::ok(())
    }

    fn retired_unsigned_snapshot(
        self: Rc<Self>,
        _params: channel_handle::RetiredUnsignedSnapshotParams,
        _results: channel_handle::RetiredUnsignedSnapshotResults,
    ) -> Promise<(), capnp::Error> {
        Promise::ok(())
    }

    fn retired_unsigned_incremental(
        self: Rc<Self>,
        _params: channel_handle::RetiredUnsignedIncrementalParams,
        _results: channel_handle::RetiredUnsignedIncrementalResults,
    ) -> Promise<(), capnp::Error> {
        Promise::ok(())
    }

    fn subscribe_context(
        self: Rc<Self>,
        _params: channel_handle::SubscribeContextParams,
        mut results: channel_handle::SubscribeContextResults,
    ) -> Promise<(), capnp::Error> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.hub
            .channels
            .lock()
            .unwrap()
            .entry(self.channel.clone())
            .or_default()
            .snapshot_requests
            .push(Subscription {
                peer: self.peer,
                tx,
            });
        let client: context_stream::Client = capnp_rpc::new_client(SnapshotRequestStream {
            rx: Rc::new(RefCell::new(rx)),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn request_document_snapshot(
        self: Rc<Self>,
        _params: channel_handle::RequestDocumentSnapshotParams,
        _results: channel_handle::RequestDocumentSnapshotResults,
    ) -> Promise<(), capnp::Error> {
        self.hub.request_snapshot(&self.channel, self.peer);
        Promise::ok(())
    }

    fn send_action(
        self: Rc<Self>,
        params: channel_handle::SendActionParams,
        _results: channel_handle::SendActionResults,
    ) -> Promise<(), capnp::Error> {
        let p = match params.get() {
            Ok(p) => p,
            Err(e) => return Promise::err(e),
        };
        let type_id = match p
            .get_type_id()
            .and_then(|s| s.to_str().map_err(|e| capnp::Error::failed(e.to_string())))
        {
            Ok(s) => s.to_owned(),
            Err(e) => return Promise::err(e),
        };
        let payload = match p.get_payload() {
            Ok(v) => v.to_vec(),
            Err(e) => return Promise::err(e),
        };
        self.hub.fanout_action(
            &self.channel,
            self.peer,
            &Action {
                type_id,
                payload,
                author: self.user_id.clone(),
            },
        );
        Promise::ok(())
    }

    fn subscribe_actions(
        self: Rc<Self>,
        _params: channel_handle::SubscribeActionsParams,
        mut results: channel_handle::SubscribeActionsResults,
    ) -> Promise<(), capnp::Error> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.hub
            .channels
            .lock()
            .unwrap()
            .entry(self.channel.clone())
            .or_default()
            .actions
            .push(Subscription {
                peer: self.peer,
                tx,
            });
        let client: action_stream::Client = capnp_rpc::new_client(ActionStream {
            rx: Rc::new(RefCell::new(rx)),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }

    /// This harness stores no committed history: answer every open with the
    /// explicit legacy mode so the route uses the incremental op stream.
    fn committed_document(
        self: Rc<Self>,
        _params: channel_handle::CommittedDocumentParams,
        mut results: channel_handle::CommittedDocumentResults,
    ) -> Promise<(), capnp::Error> {
        match runtime_collab::committed_document::Reply::Legacy.encode() {
            Ok(reply) => {
                results.get().set_reply(&reply);
                Promise::ok(())
            }
            Err(error) => Promise::err(capnp::Error::failed(error.into())),
        }
    }

    fn subscribe_action_catalogue(
        self: Rc<Self>,
        _params: channel_handle::SubscribeActionCatalogueParams,
        mut results: channel_handle::SubscribeActionCatalogueResults,
    ) -> Promise<(), capnp::Error> {
        let client: action_catalogue_stream::Client =
            capnp_rpc::new_client(EmptyActionCatalogueStream {
                initial: RefCell::new(Some(
                    serde_json::json!({"version": 1, "surfaces": []}).to_string(),
                )),
            });
        results.get().set_stream(client);
        Promise::ok(())
    }
}

struct OpStream {
    rx: Rc<RefCell<mpsc::UnboundedReceiver<Op>>>,
}
impl stream::Server for OpStream {
    #[allow(
        clippy::await_holding_refcell_ref,
        reason = "UnboundedReceiver::recv borrows the receiver for the life of the returned \
                  future, so the RefMut must stay live across the await; each stream's Rc is \
                  driven by exactly one in-flight `read` promise at a time (capnp-rpc callers \
                  await one `read` before issuing the next), so no reentrant double-borrow \
                  occurs"
    )]
    fn read(
        self: Rc<Self>,
        _params: stream::ReadParams,
        mut results: stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            let value =
                rx.borrow_mut().recv().await.ok_or_else(|| {
                    capnp::Error::disconnected("typed test op stream closed".into())
                })?;
            let mut out = results.get();
            out.set_op(&value.bytes);
            out.set_from_user_id(&value.author);
            out.set_kind(runtime_collab::raw::OpKind::Signed);
            Ok(())
        })
    }
}

/// Context stream carrying only directed snapshot requests; this harness has
/// no roster or permission snapshots to publish.
struct SnapshotRequestStream {
    rx: Rc<RefCell<mpsc::UnboundedReceiver<()>>>,
}
impl context_stream::Server for SnapshotRequestStream {
    #[allow(
        clippy::await_holding_refcell_ref,
        reason = "one in-flight `read` per stream, as for the op stream above"
    )]
    fn read(
        self: Rc<Self>,
        _params: context_stream::ReadParams,
        mut results: context_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            rx.borrow_mut().recv().await.ok_or_else(|| {
                capnp::Error::disconnected("typed test context stream closed".into())
            })?;
            results.get().init_ctx().set_snapshot_requested(true);
            Ok(())
        })
    }
}

struct PresenceHandle {
    hub: Arc<Hub>,
    peer: u64,
    user_id: String,
    channel: String,
}
impl presence_handle::Server for PresenceHandle {
    fn publish_pointer(
        self: Rc<Self>,
        params: presence_handle::PublishPointerParams,
        _results: presence_handle::PublishPointerResults,
    ) -> Promise<(), capnp::Error> {
        let p = match params.get() {
            Ok(p) => p,
            Err(e) => return Promise::err(e),
        };
        self.publish(
            p.get_key_bytes().map(<[u8]>::to_vec),
            PresenceValue::Pointer(p.get_x(), p.get_y()),
        )
    }
    fn publish_caret(
        self: Rc<Self>,
        params: presence_handle::PublishCaretParams,
        _results: presence_handle::PublishCaretResults,
    ) -> Promise<(), capnp::Error> {
        let p = match params.get() {
            Ok(p) => p,
            Err(e) => return Promise::err(e),
        };
        self.publish(
            p.get_key_bytes().map(<[u8]>::to_vec),
            PresenceValue::Caret(p.get_pos()),
        )
    }
    fn publish_selection(
        self: Rc<Self>,
        params: presence_handle::PublishSelectionParams,
        _results: presence_handle::PublishSelectionResults,
    ) -> Promise<(), capnp::Error> {
        let p = match params.get() {
            Ok(p) => p,
            Err(e) => return Promise::err(e),
        };
        self.publish(
            p.get_key_bytes().map(<[u8]>::to_vec),
            PresenceValue::Selection(p.get_start(), p.get_end()),
        )
    }
    fn publish_arrived(
        self: Rc<Self>,
        params: presence_handle::PublishArrivedParams,
        _results: presence_handle::PublishArrivedResults,
    ) -> Promise<(), capnp::Error> {
        let p = match params.get() {
            Ok(p) => p,
            Err(e) => return Promise::err(e),
        };
        self.publish(
            p.get_key_bytes().map(<[u8]>::to_vec),
            PresenceValue::Arrived,
        )
    }
    fn publish_left(
        self: Rc<Self>,
        params: presence_handle::PublishLeftParams,
        _results: presence_handle::PublishLeftResults,
    ) -> Promise<(), capnp::Error> {
        let p = match params.get() {
            Ok(p) => p,
            Err(e) => return Promise::err(e),
        };
        self.publish(p.get_key_bytes().map(<[u8]>::to_vec), PresenceValue::Left)
    }
    fn subscribe_all(
        self: Rc<Self>,
        _params: presence_handle::SubscribeAllParams,
        mut results: presence_handle::SubscribeAllResults,
    ) -> Promise<(), capnp::Error> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.hub
            .channels
            .lock()
            .unwrap()
            .entry(self.channel.clone())
            .or_default()
            .presence
            .push(Subscription {
                peer: self.peer,
                tx,
            });
        let client: presence_stream::Client = capnp_rpc::new_client(PresenceStream {
            rx: Rc::new(RefCell::new(rx)),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }
}

impl PresenceHandle {
    fn publish(
        &self,
        key: Result<Vec<u8>, capnp::Error>,
        payload: PresenceValue,
    ) -> Promise<(), capnp::Error> {
        let key = match key {
            Ok(k) => k,
            Err(e) => return Promise::err(e),
        };
        self.hub.fanout_presence(
            &self.channel,
            self.peer,
            &Presence {
                author: self.user_id.clone(),
                key,
                payload,
            },
        );
        Promise::ok(())
    }
}

struct PresenceStream {
    rx: Rc<RefCell<mpsc::UnboundedReceiver<Presence>>>,
}
impl presence_stream::Server for PresenceStream {
    #[allow(
        clippy::await_holding_refcell_ref,
        reason = "UnboundedReceiver::recv borrows the receiver for the life of the returned \
                  future, so the RefMut must stay live across the await; each stream's Rc is \
                  driven by exactly one in-flight `read` promise at a time (capnp-rpc callers \
                  await one `read` before issuing the next), so no reentrant double-borrow \
                  occurs"
    )]
    fn read(
        self: Rc<Self>,
        _params: presence_stream::ReadParams,
        mut results: presence_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            let value = rx.borrow_mut().recv().await.ok_or_else(|| {
                capnp::Error::disconnected("typed test presence stream closed".into())
            })?;
            let mut event: presence_event::Builder = results.get().init_event();
            event.set_user_id(&value.author);
            event.set_key_bytes(&value.key);
            let mut payload: presence_payload::Builder = event.init_payload();
            match value.payload {
                PresenceValue::Pointer(x, y) => {
                    let mut p = payload.init_pointer();
                    p.set_x(x);
                    p.set_y(y);
                }
                PresenceValue::Caret(pos) => payload.init_caret().set_pos(pos),
                PresenceValue::Selection(start, end) => {
                    let mut p = payload.init_selection();
                    p.set_start(start);
                    p.set_end(end);
                }
                PresenceValue::Arrived => payload.set_arrived(()),
                PresenceValue::Left => payload.set_left(()),
            }
            Ok(())
        })
    }
}

struct ActionStream {
    rx: Rc<RefCell<mpsc::UnboundedReceiver<Action>>>,
}
impl action_stream::Server for ActionStream {
    #[allow(
        clippy::await_holding_refcell_ref,
        reason = "UnboundedReceiver::recv borrows the receiver for the life of the returned \
                  future, so the RefMut must stay live across the await; each stream's Rc is \
                  driven by exactly one in-flight `read` promise at a time (capnp-rpc callers \
                  await one `read` before issuing the next), so no reentrant double-borrow \
                  occurs"
    )]
    fn read(
        self: Rc<Self>,
        _params: action_stream::ReadParams,
        mut results: action_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            let value = rx.borrow_mut().recv().await.ok_or_else(|| {
                capnp::Error::disconnected("typed test action stream closed".into())
            })?;
            let mut out = results.get();
            out.set_type_id(&value.type_id);
            out.set_payload(&value.payload);
            out.set_from_user_id(&value.author);
            Ok(())
        })
    }
}

struct EmptyActionCatalogueStream {
    initial: RefCell<Option<String>>,
}

impl action_catalogue_stream::Server for EmptyActionCatalogueStream {
    fn read(
        self: Rc<Self>,
        _params: action_catalogue_stream::ReadParams,
        mut results: action_catalogue_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let Some(initial) = self.initial.borrow_mut().take() else {
            return Promise::from_future(std::future::pending());
        };
        results.get().set_catalogue_json(&initial);
        Promise::ok(())
    }
}
