//! Integration tests for schema v2 additions:
//!
//! - Extended presence: `publish_caret`, `publish_selection`,
//!   `publish_arrived`, `publish_left`, and the unified `subscribe_all`
//!   stream carrying every payload variant.
//! - Roster/context: `subscribe_context` yielding `ChannelContext` snapshots.
//! - Signed op stream attribution: `Stream.read()` returns `(op,
//!   fromUserId)`, decoded by the wrapper.
//! - Legacy unsigned paths (`send_unsigned_snapshot` /
//!   `send_unsigned_incremental`) dispatched to the broker verbatim.
//! - Scrollback: `fetch_scrollback` decodes `ScrollbackBatch`.
//!
//! No real broker — everything runs against a hand-rolled fake server over
//! an in-memory `UnixStream` pair.

// capnp's generated `Server` traits declare each verb as `-> impl Future`,
// and these fakes return capnp's own `Promise`, which is the concrete type
// behind it. The lint's concern is a caller coming to depend on the narrower
// return; nothing outside this file constructs these servers, so the
// refinement stays where it is written.
#![allow(refining_impl_trait_internal)]
use std::cell::RefCell;
use std::os::fd::IntoRawFd;
use std::rc::Rc;

use capnp::capability::Promise;
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use tokio::net::UnixStream;
use tokio::task::LocalSet;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use runtime_collab::raw::{
    channel_context, channel_handle, collab_service, context_stream, cursor_stream, presence_event,
    presence_handle, presence_payload, presence_stream, scrollback_batch, stream,
};
use runtime_collab::{CollabClient, CollabServiceClient, PresencePayload};
use runtime_portal::UnixCapnpClient;

// shared capture state

#[derive(Default)]
struct Captures {
    unsigned_snapshots: Vec<Vec<u8>>,
    unsigned_incrementals: Vec<Vec<u8>>,
    presence_caret: Vec<(Vec<u8>, u64)>,
    presence_selection: Vec<(Vec<u8>, u64, u64)>,
    presence_arrived: Vec<Vec<u8>>,
    presence_left: Vec<Vec<u8>>,
    presence_cursor: Vec<(f32, f32)>,
    scrollback_requests: Vec<(u64, u32)>,
}

type SharedCaptures = Rc<RefCell<Captures>>;

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
    Caret { pos: u64 },
    Selection { start: u64, end: u64 },
    Arrived,
    Left,
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
            PresencePayloadKind::Caret { pos } => {
                payload.init_caret().set_pos(pos);
            }
            PresencePayloadKind::Selection { start, end } => {
                let mut b = payload.init_selection();
                b.set_start(start);
                b.set_end(end);
            }
            PresencePayloadKind::Arrived => payload.set_arrived(()),
            PresencePayloadKind::Left => payload.set_left(()),
        }
        Promise::ok(())
    }
}

// fake cursor stream (schema-v1 path)

struct FakeCursorStream {
    backlog: Rc<RefCell<Vec<(String, f32, f32)>>>,
}

impl cursor_stream::Server for FakeCursorStream {
    fn read(
        self: Rc<Self>,
        _params: cursor_stream::ReadParams,
        mut results: cursor_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let mut backlog = self.backlog.borrow_mut();
        if backlog.is_empty() {
            return Promise::err(capnp::Error::failed("cursor backlog exhausted".into()));
        }
        let (id, x, y) = backlog.remove(0);
        let mut r = results.get();
        r.set_user_id(&id);
        r.set_x(x);
        r.set_y(y);
        Promise::ok(())
    }
}

// fake presence handle

struct FakePresence {
    captures: SharedCaptures,
    presence_backlog: Rc<RefCell<Vec<SeedPresence>>>,
    cursor_backlog: Rc<RefCell<Vec<(String, f32, f32)>>>,
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

    fn subscribe_remote_cursors(
        self: Rc<Self>,
        _params: presence_handle::SubscribeRemoteCursorsParams,
        mut results: presence_handle::SubscribeRemoteCursorsResults,
    ) -> Promise<(), capnp::Error> {
        let client: cursor_stream::Client = capnp_rpc::new_client(FakeCursorStream {
            backlog: self.cursor_backlog.clone(),
        });
        results.get().set_stream(client);
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
    backlog: Rc<RefCell<Vec<(Vec<u8>, String)>>>,
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
                .init_collaborators(seed.collaborators.len() as u32);
            for (i, (uid, name, color)) in seed.collaborators.iter().enumerate() {
                let mut c = collabs.reborrow().get(i as u32);
                c.set_user_id(uid);
                c.set_display_name(name);
                c.set_color(*color);
            }
        }
        {
            let mut perms = ctx.init_permissions(seed.permissions.len() as u32);
            for (i, p) in seed.permissions.iter().enumerate() {
                perms.set(i as u32, p.as_str());
            }
        }
        Promise::ok(())
    }
}

// fake channel handle

struct FakeChannel {
    captures: SharedCaptures,
    op_backlog: Rc<RefCell<Vec<(Vec<u8>, String)>>>,
    presence_backlog: Rc<RefCell<Vec<SeedPresence>>>,
    cursor_backlog: Rc<RefCell<Vec<(String, f32, f32)>>>,
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
            cursor_backlog: self.cursor_backlog.clone(),
        });
        results.get().set_handle(client);
        Promise::ok(())
    }

    fn send_unsigned_snapshot(
        self: Rc<Self>,
        params: channel_handle::SendUnsignedSnapshotParams,
        _results: channel_handle::SendUnsignedSnapshotResults,
    ) -> Promise<(), capnp::Error> {
        let d = match params.get().and_then(runtime_collab::collab_capnp::channel_handle::send_unsigned_snapshot_params::Reader::get_data) {
            Ok(b) => b.to_vec(),
            Err(e) => return Promise::err(e),
        };
        self.captures.borrow_mut().unsigned_snapshots.push(d);
        Promise::ok(())
    }

    fn send_unsigned_incremental(
        self: Rc<Self>,
        params: channel_handle::SendUnsignedIncrementalParams,
        _results: channel_handle::SendUnsignedIncrementalResults,
    ) -> Promise<(), capnp::Error> {
        let d = match params.get().and_then(runtime_collab::collab_capnp::channel_handle::send_unsigned_incremental_params::Reader::get_data) {
            Ok(b) => b.to_vec(),
            Err(e) => return Promise::err(e),
        };
        self.captures.borrow_mut().unsigned_incrementals.push(d);
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

    fn fetch_scrollback(
        self: Rc<Self>,
        params: channel_handle::FetchScrollbackParams,
        mut results: channel_handle::FetchScrollbackResults,
    ) -> Promise<(), capnp::Error> {
        let p = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        self.captures
            .borrow_mut()
            .scrollback_requests
            .push((p.get_before_seq(), p.get_max()));
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
            let mut ops = batch.reborrow().init_ops(seed.ops.len() as u32);
            for (i, op) in seed.ops.iter().enumerate() {
                ops.set(i as u32, op);
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
    cursor_backlog: Vec<(String, f32, f32)>,
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
            cursor_backlog: Rc::new(RefCell::new(self.cursor_backlog.clone())),
            context_backlog: Rc::new(RefCell::new(self.context_backlog.clone())),
            scrollback_response: Rc::new(RefCell::new(self.scrollback_response.clone())),
        };
        let client: channel_handle::Client = capnp_rpc::new_client(chan);
        results.get().set_handle(client);
        Promise::ok(())
    }
}

async fn stand_up_client(broker: FakeBroker) -> CollabClient {
    let (server_side, client_side) = UnixStream::pair().unwrap();

    let broker_client: collab_service::Client = capnp_rpc::new_client(broker);
    let (server_read, server_write) = server_side.into_split();
    let server_network = Box::new(twoparty::VatNetwork::new(
        server_read.compat(),
        server_write.compat_write(),
        rpc_twoparty_capnp::Side::Server,
        Default::default(),
    ));
    let server_rpc = RpcSystem::new(server_network, Some(broker_client.client));
    tokio::task::spawn_local(async move {
        let _ = server_rpc.await;
    });

    let client_fd = client_side.into_std().unwrap().into_raw_fd();
    let unix =
        UnixCapnpClient::from_connected_fd(client_fd, rpc_twoparty_capnp::Side::Client).unwrap();
    let client: CollabServiceClient = unix.bootstrap();
    CollabClient::from_raw(client)
}

// tests

#[tokio::test(flavor = "current_thread")]
async fn presence_variants_publish_and_stream_round_trip() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let captures = Rc::new(RefCell::new(Captures::default()));

            // Seed one of each payload variant for full-variant coverage on `subscribe_all`.
            let presence_backlog = vec![
                SeedPresence {
                    user_id: "alice".into(),
                    key_bytes: b"key-1".to_vec(),
                    payload: PresencePayloadKind::Cursor { x: 12.5, y: -3.0 },
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
                    user_id: "dave".into(),
                    key_bytes: b"key-4".to_vec(),
                    payload: PresencePayloadKind::Arrived,
                },
                SeedPresence {
                    user_id: "eve".into(),
                    key_bytes: b"key-5".to_vec(),
                    payload: PresencePayloadKind::Left,
                },
            ];

            let broker = FakeBroker {
                captures: captures.clone(),
                op_backlog: Vec::new(),
                presence_backlog,
                cursor_backlog: Vec::new(),
                context_backlog: Vec::new(),
                scrollback_response: None,
            };
            let collab = stand_up_client(broker).await;
            let channel = collab.join_channel("c", b"id").await.unwrap();
            let presence = channel.presence().await.unwrap();

            // Publish every variant.
            presence.publish_cursor(1.5, 2.5).await.unwrap();
            presence.publish_caret(b"art-a", 100).await.unwrap();
            presence.publish_selection(b"art-b", 3, 9).await.unwrap();
            presence.publish_arrived(b"art-c").await.unwrap();
            presence.publish_left(b"art-d").await.unwrap();

            {
                let c = captures.borrow();
                assert_eq!(c.presence_cursor, vec![(1.5, 2.5)]);
                assert_eq!(c.presence_caret, vec![(b"art-a".to_vec(), 100)]);
                assert_eq!(c.presence_selection, vec![(b"art-b".to_vec(), 3, 9)]);
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
            assert_eq!(e2.user_id, "bob");
            assert_eq!(e2.payload, PresencePayload::Caret { pos: 42 });

            let e3 = stream.read().await.unwrap();
            assert_eq!(e3.user_id, "carol");
            assert_eq!(e3.payload, PresencePayload::Selection { start: 7, end: 11 });

            let e4 = stream.read().await.unwrap();
            assert_eq!(e4.user_id, "dave");
            assert_eq!(e4.payload, PresencePayload::Arrived);

            let e5 = stream.read().await.unwrap();
            assert_eq!(e5.user_id, "eve");
            assert_eq!(e5.payload, PresencePayload::Left);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn backward_compat_publish_cursor_and_cursor_stream_still_work() {
    // Guards `PresenceHandle::publish_cursor` + `subscribe_remote_cursors`
    // against drift from the schema v2 additions.
    let local = LocalSet::new();
    local
        .run_until(async {
            let captures = Rc::new(RefCell::new(Captures::default()));
            let cursor_backlog = vec![("peer-x".to_string(), 4.0, 5.0)];
            let broker = FakeBroker {
                captures: captures.clone(),
                op_backlog: Vec::new(),
                presence_backlog: Vec::new(),
                cursor_backlog,
                context_backlog: Vec::new(),
                scrollback_response: None,
            };
            let collab = stand_up_client(broker).await;
            let channel = collab.join_channel("c", b"id").await.unwrap();
            let presence = channel.presence().await.unwrap();

            presence.publish_cursor(11.0, 22.0).await.unwrap();
            assert_eq!(
                captures.borrow().presence_cursor,
                vec![(11.0, 22.0)],
                "publish_cursor kept @0 semantics"
            );

            let cursors = presence.subscribe_remote_cursors().await.unwrap();
            let u = cursors.read().await.unwrap();
            assert_eq!(u.user_id, "peer-x");
            assert_eq!((u.x, u.y), (4.0, 5.0));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn subscribe_context_delivers_typed_snapshot() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let captures = Rc::new(RefCell::new(Captures::default()));
            let context_backlog = vec![
                SeedContext {
                    collaborators: vec![
                        ("alice".into(), "Alice".into(), 0x00ff0000),
                        ("bob".into(), "Bob".into(), 0x00_00ff00),
                    ],
                    permissions: vec!["write".into(), "share".into()],
                },
                SeedContext {
                    collaborators: vec![("alice".into(), "Alice".into(), 0x00ff0000)],
                    permissions: vec!["write".into()],
                },
            ];
            let broker = FakeBroker {
                captures: captures.clone(),
                op_backlog: Vec::new(),
                presence_backlog: Vec::new(),
                cursor_backlog: Vec::new(),
                context_backlog,
                scrollback_response: None,
            };
            let collab = stand_up_client(broker).await;
            let channel = collab.join_channel("c", b"id").await.unwrap();
            let stream = channel.subscribe_context().await.unwrap();

            let s1 = stream.read().await.unwrap();
            assert_eq!(s1.collaborators.len(), 2);
            assert_eq!(s1.collaborators[0].user_id, "alice");
            assert_eq!(s1.collaborators[0].display_name, "Alice");
            assert_eq!(s1.collaborators[0].color, 0x00ff0000);
            assert_eq!(s1.collaborators[1].user_id, "bob");
            assert_eq!(s1.permissions, vec!["write", "share"]);

            let s2 = stream.read().await.unwrap();
            assert_eq!(s2.collaborators.len(), 1);
            assert_eq!(s2.permissions, vec!["write"]);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn op_stream_carries_from_user_id() {
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
                cursor_backlog: Vec::new(),
                context_backlog: Vec::new(),
                scrollback_response: None,
            };
            let collab = stand_up_client(broker).await;
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

#[tokio::test(flavor = "current_thread")]
async fn legacy_unsigned_paths_reach_broker() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let captures = Rc::new(RefCell::new(Captures::default()));
            let broker = FakeBroker {
                captures: captures.clone(),
                op_backlog: Vec::new(),
                presence_backlog: Vec::new(),
                cursor_backlog: Vec::new(),
                context_backlog: Vec::new(),
                scrollback_response: None,
            };
            let collab = stand_up_client(broker).await;
            let channel = collab.join_channel("c", b"id").await.unwrap();

            channel.send_unsigned_snapshot(b"full-doc").await.unwrap();
            channel
                .send_unsigned_incremental(b"change-1")
                .await
                .unwrap();
            channel
                .send_unsigned_incremental(b"change-2")
                .await
                .unwrap();

            let c = captures.borrow();
            assert_eq!(c.unsigned_snapshots, vec![b"full-doc".to_vec()]);
            assert_eq!(
                c.unsigned_incrementals,
                vec![b"change-1".to_vec(), b"change-2".to_vec()]
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn fetch_scrollback_round_trip_full_batch() {
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
                cursor_backlog: Vec::new(),
                context_backlog: Vec::new(),
                scrollback_response,
            };
            let collab = stand_up_client(broker).await;
            let channel = collab.join_channel("c", b"id").await.unwrap();

            let batch = channel.fetch_scrollback(50, 25).await.unwrap();
            assert_eq!(
                batch.ops,
                vec![b"op-a".to_vec(), b"op-b".to_vec(), b"op-c".to_vec()]
            );
            assert_eq!(batch.earliest_seq, 7);
            assert_eq!(batch.latest_seq, 9);
            assert!(!batch.done);
            assert_eq!(captures.borrow().scrollback_requests, vec![(50, 25)]);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn fetch_scrollback_empty_done_batch_decodes() {
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
                cursor_backlog: Vec::new(),
                context_backlog: Vec::new(),
                scrollback_response: None,
            };
            let collab = stand_up_client(broker).await;
            let channel = collab.join_channel("c", b"id").await.unwrap();

            let batch = channel.fetch_scrollback(0, 10).await.unwrap();
            assert!(batch.ops.is_empty());
            assert!(batch.done);
            assert_eq!(batch.earliest_seq, 0);
            assert_eq!(batch.latest_seq, 0);
        })
        .await;
}
