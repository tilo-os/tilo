//! Integration tests for schema v2 additions:
//!
//! - Extended presence: `publish_caret`, `publish_selection`,
//!   `publish_arrived`, `publish_left`, and the unified `subscribe_all`
//!   stream carrying every payload variant.
//! - Roster/context: `subscribe_context` yielding `ChannelContext` snapshots.
//! - Signed op stream attribution: `Stream.read()` returns `(op,
//!   fromUserId)`, decoded by the wrapper.
//! - Scrollback: `fetch_scrollback_for` decodes `ScrollbackBatch`.
//!
//! No real broker — everything runs against a hand-rolled fake server over
//! a bounded in-memory byte stream.

// capnp's generated `Server` traits declare each verb as `-> impl Future`,
// and these fakes return capnp's own `Promise`, which is the concrete type
// behind it. The lint's concern is a caller coming to depend on the narrower
// return; nothing outside this file constructs these servers, so the
// refinement stays where it is written.
#![allow(refining_impl_trait_internal)]
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
use std::rc::Rc;

use capnp::capability::Promise;
use capnp::message::ReaderOptions;
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use tokio::task::LocalSet;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use runtime_collab::raw::{
    channel_context, channel_handle, collab_service, context_stream, presence_event,
    presence_handle, presence_payload, presence_stream, scrollback_batch, stream,
};
use runtime_collab::{CollabClient, CollabServiceClient, PresencePayload};
use runtime_portal::CapnpClient;

// shared capture state

#[derive(Default)]
struct Captures {
    presence_pointer: Vec<(Vec<u8>, f32, f32)>,
    presence_caret: Vec<(Vec<u8>, u64)>,
    presence_selection: Vec<(Vec<u8>, u64, u64)>,
    presence_typing: Vec<(Vec<u8>, u64, u64)>,
    presence_arrived: Vec<Vec<u8>>,
    presence_left: Vec<Vec<u8>>,
    presence_cursor: Vec<(f32, f32)>,
    presence_status: Vec<(Vec<u8>, String, bool)>,
    scrollback_requests: Vec<(Vec<u8>, u64, u32)>,
}

type SharedCaptures = Rc<RefCell<Captures>>;

/// Pre-seeded `(op bytes, from-user attribution)` pairs shared between a
/// `FakeChannel` and the `FakeOpStream` it hands out.
type OpBacklog = Rc<RefCell<Vec<(Vec<u8>, String)>>>;

// fake presence stream

/// Pre-seeded events; each `read()` pops one, as a (user, `key_bytes`,
/// payload) triple encoded via the `PresencePayloadKind` helper below.
#[derive(Clone)]
struct SeedPresence {
    user_id: String,
    key_bytes: Vec<u8>,
    payload: PresencePayloadKind,
}

#[derive(Clone)]
enum PresencePayloadKind {
    Cursor { x: f32, y: f32 },
    Pointer { x: f32, y: f32 },
    Caret { pos: u64 },
    Selection { start: u64, end: u64 },
    Typing { start: u64, end: u64 },
    Status { text: String, automatic: bool },
    Arrived,
    Left,
}

fn every_presence_payload() -> Vec<SeedPresence> {
    vec![
        SeedPresence {
            user_id: "alice".into(),
            key_bytes: b"key-1".to_vec(),
            payload: PresencePayloadKind::Cursor { x: 12.5, y: -3.0 },
        },
        SeedPresence {
            user_id: "alice-keyed".into(),
            key_bytes: b"key-pointer".to_vec(),
            payload: PresencePayloadKind::Pointer { x: 3.5, y: 8.0 },
        },
        SeedPresence {
            user_id: "bob".into(),
            key_bytes: b"key-2".to_vec(),
            payload: PresencePayloadKind::Caret { pos: 42 },
        },
        SeedPresence {
            user_id: "carol".into(),
            key_bytes: b"key-3".to_vec(),
            payload: PresencePayloadKind::Selection { start: 7, end: 11 },
        },
        SeedPresence {
            user_id: "carol-typing".into(),
            key_bytes: b"key-typing".to_vec(),
            payload: PresencePayloadKind::Typing { start: 12, end: 15 },
        },
        SeedPresence {
            user_id: "frank".into(),
            key_bytes: b"key-status".to_vec(),
            payload: PresencePayloadKind::Status {
                text: "Reconciling forecast".into(),
                automatic: true,
            },
        },
        SeedPresence {
            user_id: "dave".into(),
            key_bytes: b"key-4".to_vec(),
            payload: PresencePayloadKind::Arrived,
        },
        SeedPresence {
            user_id: "eve".into(),
            key_bytes: b"key-5".to_vec(),
            payload: PresencePayloadKind::Left,
        },
    ]
}

struct FakePresenceStream {
    backlog: Rc<RefCell<Vec<SeedPresence>>>,
}

impl presence_stream::Server for FakePresenceStream {
    fn read(
        self: Rc<Self>,
        _params: presence_stream::ReadParams,
        mut results: presence_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let mut backlog = self.backlog.borrow_mut();
        if backlog.is_empty() {
            return Promise::err(capnp::Error::failed("presence backlog exhausted".into()));
        }
        let seed = backlog.remove(0);
        let mut ev: presence_event::Builder = results.get().init_event();
        ev.set_user_id(&seed.user_id);
        ev.set_key_bytes(&seed.key_bytes);
        let mut payload: presence_payload::Builder = ev.init_payload();
        match seed.payload {
            PresencePayloadKind::Cursor { x, y } => {
                let mut b = payload.init_cursor();
                b.set_x(x);
                b.set_y(y);
            }
            PresencePayloadKind::Pointer { x, y } => {
                let mut b = payload.init_pointer();
                b.set_x(x);
                b.set_y(y);
            }
            PresencePayloadKind::Caret { pos } => {
                payload.init_caret().set_pos(pos);
            }
            PresencePayloadKind::Selection { start, end } => {
                let mut b = payload.init_selection();
                b.set_start(start);
                b.set_end(end);
            }
            PresencePayloadKind::Typing { start, end } => {
                let mut b = payload.init_typing();
                b.set_start(start);
                b.set_end(end);
            }
            PresencePayloadKind::Status { text, automatic } => {
                let mut b = payload.init_status();
                b.set_text(&text);
                b.set_automatic(automatic);
            }
            PresencePayloadKind::Arrived => payload.set_arrived(()),
            PresencePayloadKind::Left => payload.set_left(()),
        }
        Promise::ok(())
    }
}

// fake presence handle

struct FakePresence {
    captures: SharedCaptures,
    presence_backlog: Rc<RefCell<Vec<SeedPresence>>>,
}

impl presence_handle::Server for FakePresence {
    fn publish_cursor(
        self: Rc<Self>,
        params: presence_handle::PublishCursorParams,
        _results: presence_handle::PublishCursorResults,
    ) -> Promise<(), capnp::Error> {
        let r = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        self.captures
            .borrow_mut()
            .presence_cursor
            .push((r.get_x(), r.get_y()));
        Promise::ok(())
    }

    fn publish_pointer(
        self: Rc<Self>,
        params: presence_handle::PublishPointerParams,
        _results: presence_handle::PublishPointerResults,
    ) -> Promise<(), capnp::Error> {
        let r = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        let key = match r.get_key_bytes() {
            Ok(b) => b.to_vec(),
            Err(e) => return Promise::err(e),
        };
        self.captures
            .borrow_mut()
            .presence_pointer
            .push((key, r.get_x(), r.get_y()));
        Promise::ok(())
    }

    fn publish_caret(
        self: Rc<Self>,
        params: presence_handle::PublishCaretParams,
        _results: presence_handle::PublishCaretResults,
    ) -> Promise<(), capnp::Error> {
        let r = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        let key = match r.get_key_bytes() {
            Ok(b) => b.to_vec(),
            Err(e) => return Promise::err(e),
        };
        self.captures
            .borrow_mut()
            .presence_caret
            .push((key, r.get_pos()));
        Promise::ok(())
    }

    fn publish_selection(
        self: Rc<Self>,
        params: presence_handle::PublishSelectionParams,
        _results: presence_handle::PublishSelectionResults,
    ) -> Promise<(), capnp::Error> {
        let r = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        let key = match r.get_key_bytes() {
            Ok(b) => b.to_vec(),
            Err(e) => return Promise::err(e),
        };
        self.captures
            .borrow_mut()
            .presence_selection
            .push((key, r.get_start(), r.get_end()));
        Promise::ok(())
    }

    fn publish_typing(
        self: Rc<Self>,
        params: presence_handle::PublishTypingParams,
        _results: presence_handle::PublishTypingResults,
    ) -> Promise<(), capnp::Error> {
        let r = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        let key = match r.get_key_bytes() {
            Ok(b) => b.to_vec(),
            Err(e) => return Promise::err(e),
        };
        self.captures
            .borrow_mut()
            .presence_typing
            .push((key, r.get_start(), r.get_end()));
        Promise::ok(())
    }

    fn publish_arrived(
        self: Rc<Self>,
        params: presence_handle::PublishArrivedParams,
        _results: presence_handle::PublishArrivedResults,
    ) -> Promise<(), capnp::Error> {
        let r = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        let key = match r.get_key_bytes() {
            Ok(b) => b.to_vec(),
            Err(e) => return Promise::err(e),
        };
        self.captures.borrow_mut().presence_arrived.push(key);
        Promise::ok(())
    }

    fn publish_left(
        self: Rc<Self>,
        params: presence_handle::PublishLeftParams,
        _results: presence_handle::PublishLeftResults,
    ) -> Promise<(), capnp::Error> {
        let r = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        let key = match r.get_key_bytes() {
            Ok(b) => b.to_vec(),
            Err(e) => return Promise::err(e),
        };
        self.captures.borrow_mut().presence_left.push(key);
        Promise::ok(())
    }

    fn publish_status(
        self: Rc<Self>,
        params: presence_handle::PublishStatusParams,
        _results: presence_handle::PublishStatusResults,
    ) -> Promise<(), capnp::Error> {
        let r = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        let key = match r.get_key_bytes() {
            Ok(b) => b.to_vec(),
            Err(e) => return Promise::err(e),
        };
        let text_reader = match r.get_text() {
            Ok(text) => text,
            Err(e) => return Promise::err(e),
        };
        let text = match text_reader.to_str() {
            Ok(text) => text.to_owned(),
            Err(e) => return Promise::err(capnp::Error::failed(e.to_string())),
        };
        self.captures
            .borrow_mut()
            .presence_status
            .push((key, text, r.get_automatic()));
        Promise::ok(())
    }

    fn subscribe_all(
        self: Rc<Self>,
        _params: presence_handle::SubscribeAllParams,
        mut results: presence_handle::SubscribeAllResults,
    ) -> Promise<(), capnp::Error> {
        let client: presence_stream::Client = capnp_rpc::new_client(FakePresenceStream {
            backlog: self.presence_backlog.clone(),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }
}

// fake op stream (for `subscribe`) — attribution round-trip

struct FakeOpStream {
    backlog: OpBacklog,
}

impl stream::Server for FakeOpStream {
    fn read(
        self: Rc<Self>,
        _params: stream::ReadParams,
        mut results: stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let mut backlog = self.backlog.borrow_mut();
        if backlog.is_empty() {
            return Promise::err(capnp::Error::failed("op backlog exhausted".into()));
        }
        let (op, from) = backlog.remove(0);
        let mut r = results.get();
        r.set_op(&op);
        r.set_from_user_id(&from);
        Promise::ok(())
    }
}

// fake context stream

/// A seeded `ChannelContext` snapshot to emit on the next `read()`, kept as
/// owned data so the test can pass typed values in.
#[derive(Clone)]
struct SeedContext {
    collaborators: Vec<(String, String, u32)>,
    permissions: Vec<String>,
}

struct FakeContextStream {
    backlog: Rc<RefCell<Vec<SeedContext>>>,
}

impl context_stream::Server for FakeContextStream {
    fn read(
        self: Rc<Self>,
        _params: context_stream::ReadParams,
        mut results: context_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let mut backlog = self.backlog.borrow_mut();
        if backlog.is_empty() {
            return Promise::err(capnp::Error::failed("context backlog exhausted".into()));
        }
        let seed = backlog.remove(0);
        let mut ctx: channel_context::Builder = results.get().init_ctx();
        {
            let mut collabs = ctx
                .reborrow()
                .init_collaborators(u32::try_from(seed.collaborators.len()).unwrap());
            for (i, (uid, name, color)) in seed.collaborators.iter().enumerate() {
                let mut c = collabs.reborrow().get(u32::try_from(i).unwrap());
                c.set_user_id(uid);
                c.set_display_name(name);
                c.set_color(*color);
            }
        }
        {
            let mut perms = ctx.init_permissions(u32::try_from(seed.permissions.len()).unwrap());
            for (i, p) in seed.permissions.iter().enumerate() {
                perms.set(u32::try_from(i).unwrap(), p.as_str());
            }
        }
        Promise::ok(())
    }
}

// fake channel handle

struct FakeChannel {
    captures: SharedCaptures,
    op_backlog: OpBacklog,
    presence_backlog: Rc<RefCell<Vec<SeedPresence>>>,
    context_backlog: Rc<RefCell<Vec<SeedContext>>>,
    scrollback_response: Rc<RefCell<Option<SeedScrollback>>>,
}

#[derive(Clone)]
struct SeedScrollback {
    ops: Vec<Vec<u8>>,
    earliest_seq: u64,
    latest_seq: u64,
    done: bool,
}

impl channel_handle::Server for FakeChannel {
    fn send(
        self: Rc<Self>,
        _params: channel_handle::SendParams,
        _results: channel_handle::SendResults,
    ) -> Promise<(), capnp::Error> {
        // Not exercised in schema_v2 — mock_broker.rs already covers it.
        Promise::ok(())
    }

    fn subscribe(
        self: Rc<Self>,
        _params: channel_handle::SubscribeParams,
        mut results: channel_handle::SubscribeResults,
    ) -> Promise<(), capnp::Error> {
        let client: stream::Client = capnp_rpc::new_client(FakeOpStream {
            backlog: self.op_backlog.clone(),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn presence(
        self: Rc<Self>,
        _params: channel_handle::PresenceParams,
        mut results: channel_handle::PresenceResults,
    ) -> Promise<(), capnp::Error> {
        let client: presence_handle::Client = capnp_rpc::new_client(FakePresence {
            captures: self.captures.clone(),
            presence_backlog: self.presence_backlog.clone(),
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
        let client: context_stream::Client = capnp_rpc::new_client(FakeContextStream {
            backlog: self.context_backlog.clone(),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn fetch_scrollback_for(
        self: Rc<Self>,
        params: channel_handle::FetchScrollbackForParams,
        mut results: channel_handle::FetchScrollbackForResults,
    ) -> Promise<(), capnp::Error> {
        let p = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        self.captures.borrow_mut().scrollback_requests.push((
            p.get_artifact_id()
                .map_or_else(|_| Vec::new(), <[u8]>::to_vec),
            p.get_before_seq(),
            p.get_max(),
        ));
        let seed = self
            .scrollback_response
            .borrow()
            .clone()
            .unwrap_or(SeedScrollback {
                ops: Vec::new(),
                earliest_seq: 0,
                latest_seq: 0,
                done: true,
            });
        let mut batch: scrollback_batch::Builder = results.get().init_batch();
        {
            let mut ops = batch
                .reborrow()
                .init_ops(u32::try_from(seed.ops.len()).unwrap());
            for (i, op) in seed.ops.iter().enumerate() {
                ops.set(u32::try_from(i).unwrap(), op);
            }
        }
        batch.set_earliest_seq(seed.earliest_seq);
        batch.set_latest_seq(seed.latest_seq);
        batch.set_done(seed.done);
        Promise::ok(())
    }
}

// fake broker + wiring

struct FakeBroker {
    captures: SharedCaptures,
    op_backlog: Vec<(Vec<u8>, String)>,
    presence_backlog: Vec<SeedPresence>,
    context_backlog: Vec<SeedContext>,
    scrollback_response: Option<SeedScrollback>,
}

impl collab_service::Server for FakeBroker {
    fn join_channel(
        self: Rc<Self>,
        _params: collab_service::JoinChannelParams,
        mut results: collab_service::JoinChannelResults,
    ) -> Promise<(), capnp::Error> {
        let chan = FakeChannel {
            captures: self.captures.clone(),
            op_backlog: Rc::new(RefCell::new(self.op_backlog.clone())),
            presence_backlog: Rc::new(RefCell::new(self.presence_backlog.clone())),
            context_backlog: Rc::new(RefCell::new(self.context_backlog.clone())),
            scrollback_response: Rc::new(RefCell::new(self.scrollback_response.clone())),
        };
        let client: channel_handle::Client = capnp_rpc::new_client(chan);
        results.get().set_handle(client);
        Promise::ok(())
    }
}

fn stand_up_client(broker: FakeBroker) -> CollabClient {
    let (server_side, client_side) = tokio::io::duplex(512);

    let broker_client: collab_service::Client = capnp_rpc::new_client(broker);
    let (server_read, server_write) = tokio::io::split(server_side);
    let server_network = Box::new(twoparty::VatNetwork::new(
        server_read.compat(),
        server_write.compat_write(),
        rpc_twoparty_capnp::Side::Server,
        ReaderOptions::default(),
    ));
    let server_rpc = RpcSystem::new(server_network, Some(broker_client.client));
    tokio::task::spawn_local(async move {
        let _ = server_rpc.await;
    });

    let connection = CapnpClient::from_connected_io(client_side, rpc_twoparty_capnp::Side::Client);
    let client: CollabServiceClient = connection.bootstrap();
    CollabClient::from_raw(client)
}

// tests

#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
async fn presence_and_cursor_publishes_round_trip() {
    // phase: presence_variants_publish_and_stream_round_trip
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures = Rc::new(RefCell::new(Captures::default()));

                let broker = FakeBroker {
                    captures: captures.clone(),
                    op_backlog: Vec::new(),
                    presence_backlog: every_presence_payload(),
                    context_backlog: Vec::new(),
                    scrollback_response: None,
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("c", b"id").await.unwrap();
                let presence = channel.presence().await.unwrap();

                // Publish every variant.
                presence.publish_cursor(1.5, 2.5).await.unwrap();
                presence
                    .publish_pointer(b"art-pointer", 4.0, 5.0)
                    .await
                    .unwrap();
                presence.publish_caret(b"art-a", 100).await.unwrap();
                presence.publish_selection(b"art-b", 3, 9).await.unwrap();
                presence.publish_typing(b"art-typing", 6, 10).await.unwrap();
                presence
                    .publish_status(b"room", "Reconciling forecast", true)
                    .await
                    .unwrap();
                presence.publish_arrived(b"art-c").await.unwrap();
                presence.publish_left(b"art-d").await.unwrap();

                {
                    let c = captures.borrow();
                    assert_eq!(c.presence_cursor, vec![(1.5, 2.5)]);
                    assert_eq!(
                        c.presence_pointer,
                        vec![(b"art-pointer".to_vec(), 4.0, 5.0)]
                    );
                    assert_eq!(c.presence_caret, vec![(b"art-a".to_vec(), 100)]);
                    assert_eq!(c.presence_selection, vec![(b"art-b".to_vec(), 3, 9)]);
                    assert_eq!(c.presence_typing, vec![(b"art-typing".to_vec(), 6, 10)]);
                    assert_eq!(
                        c.presence_status,
                        vec![(b"room".to_vec(), "Reconciling forecast".into(), true)]
                    );
                    assert_eq!(c.presence_arrived, vec![b"art-c".to_vec()]);
                    assert_eq!(c.presence_left, vec![b"art-d".to_vec()]);
                }

                // Drain the unified stream and assert every seeded variant decodes into
                // the right shape.
                let stream = presence.subscribe_all().await.unwrap();

                let e1 = stream.read().await.unwrap();
                assert_eq!(e1.user_id, "alice");
                assert_eq!(e1.key_bytes, b"key-1");
                assert_eq!(e1.payload, PresencePayload::Cursor { x: 12.5, y: -3.0 });

                let e2 = stream.read().await.unwrap();
                assert_eq!(e2.user_id, "alice-keyed");
                assert_eq!(e2.key_bytes, b"key-pointer");
                assert_eq!(e2.payload, PresencePayload::Pointer { x: 3.5, y: 8.0 });

                let e3 = stream.read().await.unwrap();
                assert_eq!(e3.user_id, "bob");
                assert_eq!(e3.payload, PresencePayload::Caret { pos: 42 });

                let e4 = stream.read().await.unwrap();
                assert_eq!(e4.user_id, "carol");
                assert_eq!(e4.payload, PresencePayload::Selection { start: 7, end: 11 });

                let e5 = stream.read().await.unwrap();
                assert_eq!(e5.user_id, "carol-typing");
                assert_eq!(e5.key_bytes, b"key-typing");
                assert_eq!(e5.payload, PresencePayload::Typing { start: 12, end: 15 });

                let e6 = stream.read().await.unwrap();
                assert_eq!(e6.user_id, "frank");
                assert_eq!(
                    e6.payload,
                    PresencePayload::Status {
                        text: "Reconciling forecast".into(),
                        automatic: true
                    }
                );

                let e7 = stream.read().await.unwrap();
                assert_eq!(e7.user_id, "dave");
                assert_eq!(e7.payload, PresencePayload::Arrived);

                let e8 = stream.read().await.unwrap();
                assert_eq!(e8.user_id, "eve");
                assert_eq!(e8.payload, PresencePayload::Left);
            })
            .await;
    }
    // phase: keyless_publish_cursor_still_works
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures = Rc::new(RefCell::new(Captures::default()));
                let broker = FakeBroker {
                    captures: captures.clone(),
                    op_backlog: Vec::new(),
                    presence_backlog: Vec::new(),
                    context_backlog: Vec::new(),
                    scrollback_response: None,
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("c", b"id").await.unwrap();
                let presence = channel.presence().await.unwrap();

                presence.publish_cursor(11.0, 22.0).await.unwrap();
                assert_eq!(
                    captures.borrow().presence_cursor,
                    vec![(11.0, 22.0)],
                    "publish_cursor kept @0 semantics"
                );
            })
            .await;
    }
}

#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
async fn subscriptions_and_scrollback_decode_typed_batches() {
    // phase: subscribe_context_delivers_typed_snapshot
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures = Rc::new(RefCell::new(Captures::default()));
                let context_backlog = vec![
                    SeedContext {
                        collaborators: vec![
                            ("alice".into(), "Alice".into(), 0x00ff_0000),
                            ("bob".into(), "Bob".into(), 0x00_00ff00),
                        ],
                        permissions: vec!["write".into(), "share".into()],
                    },
                    SeedContext {
                        collaborators: vec![("alice".into(), "Alice".into(), 0x00ff_0000)],
                        permissions: vec!["write".into()],
                    },
                ];
                let broker = FakeBroker {
                    captures: captures.clone(),
                    op_backlog: Vec::new(),
                    presence_backlog: Vec::new(),
                    context_backlog,
                    scrollback_response: None,
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("c", b"id").await.unwrap();
                let stream = channel.subscribe_context().await.unwrap();

                let s1 = stream.read().await.unwrap();
                assert_eq!(s1.collaborators.len(), 2);
                assert_eq!(s1.collaborators[0].user_id, "alice");
                assert_eq!(s1.collaborators[0].display_name, "Alice");
                assert_eq!(s1.collaborators[0].color, 0x00ff_0000);
                assert_eq!(s1.collaborators[1].user_id, "bob");
                assert_eq!(s1.permissions, vec!["write", "share"]);

                let s2 = stream.read().await.unwrap();
                assert_eq!(s2.collaborators.len(), 1);
                assert_eq!(s2.permissions, vec!["write"]);
            })
            .await;
    }
    // phase: op_stream_carries_from_user_id
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures = Rc::new(RefCell::new(Captures::default()));
                let op_backlog = vec![
                    (b"first-op".to_vec(), "alice".to_string()),
                    (b"second-op".to_vec(), "peer-42".to_string()),
                ];
                let broker = FakeBroker {
                    captures: captures.clone(),
                    op_backlog,
                    presence_backlog: Vec::new(),
                    context_backlog: Vec::new(),
                    scrollback_response: None,
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("c", b"id").await.unwrap();
                let stream = channel.subscribe().await.unwrap();

                let ev1 = stream.read().await.unwrap();
                assert_eq!(ev1.op, b"first-op");
                assert_eq!(ev1.from_user_id, "alice");

                let ev2 = stream.read().await.unwrap();
                assert_eq!(ev2.op, b"second-op");
                assert_eq!(ev2.from_user_id, "peer-42");
            })
            .await;
    }
    // phase: fetch_scrollback_round_trip_full_batch
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures = Rc::new(RefCell::new(Captures::default()));
                let scrollback_response = Some(SeedScrollback {
                    ops: vec![b"op-a".to_vec(), b"op-b".to_vec(), b"op-c".to_vec()],
                    earliest_seq: 7,
                    latest_seq: 9,
                    done: false,
                });
                let broker = FakeBroker {
                    captures: captures.clone(),
                    op_backlog: Vec::new(),
                    presence_backlog: Vec::new(),
                    context_backlog: Vec::new(),
                    scrollback_response,
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("c", b"id").await.unwrap();

                let artifact_id = [7_u8; 16];
                let batch = channel
                    .fetch_scrollback_for(&artifact_id, 50, 25)
                    .await
                    .unwrap();
                assert_eq!(
                    batch.ops,
                    vec![b"op-a".to_vec(), b"op-b".to_vec(), b"op-c".to_vec()]
                );
                assert_eq!(batch.earliest_seq, 7);
                assert_eq!(batch.latest_seq, 9);
                assert!(!batch.done);
                assert_eq!(
                    captures.borrow().scrollback_requests,
                    vec![(artifact_id.to_vec(), 50, 25)]
                );
            })
            .await;
    }
    // phase: fetch_scrollback_empty_done_batch_decodes
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures = Rc::new(RefCell::new(Captures::default()));
                // No scrollback_response means the fake replies with an empty+done batch —
                // the "broker holds nothing" path clients must distinguish from "here's a page".
                let broker = FakeBroker {
                    captures: captures.clone(),
                    op_backlog: Vec::new(),
                    presence_backlog: Vec::new(),
                    context_backlog: Vec::new(),
                    scrollback_response: None,
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("c", b"id").await.unwrap();

                let batch = channel
                    .fetch_scrollback_for(&[8_u8; 16], 0, 10)
                    .await
                    .unwrap();
                assert!(batch.ops.is_empty());
                assert!(batch.done);
                assert_eq!(batch.earliest_seq, 0);
                assert_eq!(batch.latest_seq, 0);
            })
            .await;
    }
}
