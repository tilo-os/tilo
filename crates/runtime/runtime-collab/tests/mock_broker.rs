//! Integration tests: end-to-end RPC over a bounded byte stream and Unix socket
//! against an in-memory `CollabService` server.
//!
//! Exercises the `UnixCapnpClient` plumbing through `runtime-portal`, the
//! generated `CollabService` schema, and the idiomatic
//! `CollabClient::join_channel` / `ChannelHandle::send` / `subscribe` /
//! `OpStream::read` wrappers.
//!
//! The broker: any `channel_id` joins and gets one `ChannelHandle`;
//! `send` stores the op in a `Vec<Vec<u8>>`; `subscribe` yields the stored
//! ops in order, and further reads after exhaustion block forever
//! (unexercised here).

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
#[cfg(unix)]
use std::os::fd::IntoRawFd;
use std::rc::Rc;

use capnp::capability::Promise;
use capnp::message::ReaderOptions;
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::task::LocalSet;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use runtime_collab::raw::{channel_handle, collab_service, presence_handle, stream};
use runtime_collab::{CollabClient, CollabServiceClient};
use runtime_portal::CapnpClient;

// server impls

struct FakeStream {
    backlog: Rc<RefCell<Vec<Vec<u8>>>>,
}

impl stream::Server for FakeStream {
    fn read(
        self: Rc<Self>,
        _params: stream::ReadParams,
        mut results: stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let mut backlog = self.backlog.borrow_mut();
        if backlog.is_empty() {
            // Never resolves — the test never exhausts the backlog, so this path is
            // unreachable in practice.
            return Promise::err(capnp::Error::failed("backlog exhausted".into()));
        }
        let next = backlog.remove(0);
        let mut r = results.get();
        r.set_op(&next[..]);
        r.set_from_user_id("fake-broker");
        Promise::ok(())
    }
}

struct FakePresence;

impl presence_handle::Server for FakePresence {
    fn publish_cursor(
        self: Rc<Self>,
        _params: presence_handle::PublishCursorParams,
        _results: presence_handle::PublishCursorResults,
    ) -> Promise<(), capnp::Error> {
        Promise::ok(())
    }
}

struct FakeChannel {
    backlog: Rc<RefCell<Vec<Vec<u8>>>>,
    captured_ops: Rc<RefCell<Vec<Vec<u8>>>>,
}

impl channel_handle::Server for FakeChannel {
    fn send(
        self: Rc<Self>,
        params: channel_handle::SendParams,
        _results: channel_handle::SendResults,
    ) -> Promise<(), capnp::Error> {
        let op = match params
            .get()
            .and_then(runtime_collab::collab_capnp::channel_handle::send_params::Reader::get_op)
        {
            Ok(o) => o.to_vec(),
            Err(e) => return Promise::err(e),
        };
        self.captured_ops.borrow_mut().push(op);
        Promise::ok(())
    }

    fn subscribe(
        self: Rc<Self>,
        _params: channel_handle::SubscribeParams,
        mut results: channel_handle::SubscribeResults,
    ) -> Promise<(), capnp::Error> {
        let stream = FakeStream {
            backlog: self.backlog.clone(),
        };
        let client: stream::Client = capnp_rpc::new_client(stream);
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn presence(
        self: Rc<Self>,
        _params: channel_handle::PresenceParams,
        mut results: channel_handle::PresenceResults,
    ) -> Promise<(), capnp::Error> {
        let client: presence_handle::Client = capnp_rpc::new_client(FakePresence);
        results.get().set_handle(client);
        Promise::ok(())
    }
}

/// `(channel_id, identity bytes)` pairs captured from every `join_channel` call.
type CapturedJoins = Rc<RefCell<Vec<(String, Vec<u8>)>>>;

struct FakeBroker {
    captured_joins: CapturedJoins,
    captured_ops: Rc<RefCell<Vec<Vec<u8>>>>,
    seed_backlog: Vec<Vec<u8>>,
}

impl collab_service::Server for FakeBroker {
    fn join_channel(
        self: Rc<Self>,
        params: collab_service::JoinChannelParams,
        mut results: collab_service::JoinChannelResults,
    ) -> Promise<(), capnp::Error> {
        let reader = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        let chan = match reader.get_channel_id().and_then(|t| {
            t.to_str()
                .map(std::borrow::ToOwned::to_owned)
                .map_err(|e| capnp::Error::failed(format!("bad utf8: {e}")))
        }) {
            Ok(s) => s,
            Err(e) => return Promise::err(e),
        };
        let identity = match reader.get_identity() {
            Ok(b) => b.to_vec(),
            Err(e) => return Promise::err(e),
        };
        self.captured_joins.borrow_mut().push((chan, identity));

        let chan_server = FakeChannel {
            backlog: Rc::new(RefCell::new(self.seed_backlog.clone())),
            captured_ops: self.captured_ops.clone(),
        };
        let client: channel_handle::Client = capnp_rpc::new_client(chan_server);
        results.get().set_handle(client);
        Promise::ok(())
    }
}

// the test itself

#[tokio::test(flavor = "current_thread")]
async fn connected_io_join_send_subscribe_read() {
    let (server_side, client_side) = tokio::io::duplex(512);
    let connection = CapnpClient::from_connected_io(client_side, rpc_twoparty_capnp::Side::Client);
    assert_round_trip(server_side, connection).await;
}

#[cfg(windows)]
#[tokio::test(flavor = "current_thread")]
async fn named_pipe_join_send_subscribe_read() {
    use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!(r"\\.\pipe\tilo-rpc-test-{}-{nonce}", std::process::id());
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .create(&name)
        .expect("test-owned named pipe");
    let client = ClientOptions::new().open(&name).expect("connect test pipe");
    tilo_test_env::within("the named pipe connection", server.connect())
        .await
        .expect("named pipe connected");
    let connection = CapnpClient::from_connected_io(client, rpc_twoparty_capnp::Side::Client);
    tilo_test_env::within(
        "the named pipe round trip",
        assert_round_trip(server, connection),
    )
    .await;
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn round_trip_join_send_subscribe_read() {
    let (server_side, client_side) = UnixStream::pair().unwrap();
    let fd = client_side.into_std().unwrap().into_raw_fd();
    let connection = CapnpClient::from_connected_fd(fd, rpc_twoparty_capnp::Side::Client).unwrap();
    assert_round_trip(server_side, connection).await;
}

/// Two clients of the one launcher-inherited socket — an app's own manager
/// beside the framework's, or a room and a record manager — each get a
/// working connection; neither owns the inherited descriptor, so dropping
/// one never closes it under the other (which used to corrupt both RPC
/// streams and then abort with "owned file descriptor already closed").
#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn clients_share_the_inherited_connection() {
    use std::os::fd::AsRawFd;

    let (server_side, inherited) = UnixStream::pair().unwrap();
    let inherited = inherited.into_std().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove("TILO_RUNTIME_CONNECTION_OWNER_PID")
        .set("TILO_SVC_COLLAB_FD", inherited.as_raw_fd().to_string());

    LocalSet::new()
        .run_until(async {
            let captured_joins = CapturedJoins::default();
            let captured_ops = Rc::new(RefCell::new(Vec::<Vec<u8>>::new()));
            let broker: collab_service::Client = capnp_rpc::new_client(FakeBroker {
                captured_joins: captured_joins.clone(),
                captured_ops: captured_ops.clone(),
                seed_backlog: Vec::new(),
            });
            let (read, write) = tokio::io::split(server_side);
            let server = RpcSystem::new(
                Box::new(twoparty::VatNetwork::new(
                    read.compat(),
                    write.compat_write(),
                    rpc_twoparty_capnp::Side::Server,
                    ReaderOptions::default(),
                )),
                Some(broker.client),
            );
            tokio::task::spawn_local(async move {
                let _ = server.await;
            });

            let deadline = std::time::Duration::from_secs(10);
            let first = runtime_collab::try_connect().expect("first client");
            let second = runtime_collab::try_connect().expect("second client");
            let room = tokio::time::timeout(deadline, first.join_channel("room", b"first"))
                .await
                .expect("first join deadline")
                .expect("first join");
            let record = tokio::time::timeout(deadline, second.join_channel("record", b"second"))
                .await
                .expect("second join deadline")
                .expect("second join");
            room.send(b"room-op").await.expect("room send");
            drop((room, first));
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            tokio::time::timeout(deadline, record.send(b"record-op"))
                .await
                .expect("send after the other client dropped")
                .expect("record send");
            let third = runtime_collab::try_connect().expect("a later client");
            tokio::time::timeout(deadline, third.join_channel("later", b"third"))
                .await
                .expect("third join deadline")
                .expect("third join");

            let joins = captured_joins.borrow().clone();
            assert_eq!(
                joins,
                vec![
                    ("room".to_owned(), b"first".to_vec()),
                    ("record".to_owned(), b"second".to_vec()),
                    ("later".to_owned(), b"third".to_vec()),
                ]
            );
            assert_eq!(
                *captured_ops.borrow(),
                vec![b"room-op".to_vec(), b"record-op".to_vec()]
            );
        })
        .await;

    // The inherited descriptor is still open, now close-on-exec.
    // SAFETY: fcntl only inspects the descriptor `inherited` owns.
    let flags = unsafe { libc::fcntl(inherited.as_raw_fd(), libc::F_GETFD) };
    assert!(flags >= 0, "the inherited descriptor was closed");
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
}

#[expect(
    clippy::future_not_send,
    reason = "Cap'n Proto RPC is single-threaded; the round trip runs on a LocalSet"
)]
async fn assert_round_trip<S>(server_side: S, connection: CapnpClient)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + 'static,
{
    let local = LocalSet::new();
    local
        .run_until(async {
            // 2. Stand up the server RPC system.
            let captured_joins = Rc::new(RefCell::new(Vec::<(String, Vec<u8>)>::new()));
            let captured_ops = Rc::new(RefCell::new(Vec::<Vec<u8>>::new()));
            let seed_backlog: Vec<Vec<u8>> = vec![b"first-op".to_vec(), b"second-op".to_vec()];

            let broker = FakeBroker {
                captured_joins: captured_joins.clone(),
                captured_ops: captured_ops.clone(),
                seed_backlog,
            };
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

            // Both transports enter the same production RPC driver and typed SDK.
            let client: CollabServiceClient = connection.bootstrap();
            let collab = CollabClient::from_raw(client);

            // 4. join_channel
            let identity_blob = b"identity-blob".to_vec();
            let channel = collab
                .join_channel("test-channel", &identity_blob)
                .await
                .expect("join_channel");

            // 5. send a couple ops
            channel.send(b"hello").await.expect("send 1");
            channel.send(b"world").await.expect("send 2");

            // 6. subscribe + read both seeded ops
            let stream = channel.subscribe().await.expect("subscribe");
            let op1 = stream.read().await.expect("read 1");
            let op2 = stream.read().await.expect("read 2");

            // 7. assertions
            assert_eq!(op1.op, b"first-op");
            assert_eq!(op1.from_user_id, "fake-broker");
            assert_eq!(op2.op, b"second-op");
            assert_eq!(op2.from_user_id, "fake-broker");
            let joins = captured_joins.borrow().clone();
            assert_eq!(joins.len(), 1);
            assert_eq!(joins[0].0, "test-channel");
            assert_eq!(joins[0].1, identity_blob);
            let ops = captured_ops.borrow().clone();
            assert_eq!(ops, vec![b"hello".to_vec(), b"world".to_vec()]);
        })
        .await;
}
