//! Integration test: end-to-end RPC round-trip over a real Unix socket
//! against a hand-rolled fake `CollabService` server.
//!
//! Exercises the `UnixCapnpClient` plumbing through `runtime-portal`, the
//! generated `CollabService` schema, and the idiomatic
//! `CollabClient::join_channel` / `ChannelHandle::send` / `subscribe` /
//! `OpStream::read` wrappers.
//!
//! The fake broker: any `channel_id` joins and gets one `ChannelHandle`;
//! `send` stores the op in a `Vec<Vec<u8>>`; `subscribe` yields the stored
//! ops in order, and further reads after exhaustion block forever
//! (unexercised here).

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

use runtime_collab::raw::{channel_handle, collab_service, presence_handle, stream};
use runtime_collab::{CollabClient, CollabServiceClient};
use runtime_portal::UnixCapnpClient;

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

    fn subscribe_remote_cursors(
        self: Rc<Self>,
        _params: presence_handle::SubscribeRemoteCursorsParams,
        _results: presence_handle::SubscribeRemoteCursorsResults,
    ) -> Promise<(), capnp::Error> {
        // Not exercised in this test.
        Promise::err(capnp::Error::unimplemented(
            "subscribe_remote_cursors".into(),
        ))
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

struct FakeBroker {
    captured_joins: Rc<RefCell<Vec<(String, Vec<u8>)>>>,
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
async fn round_trip_join_send_subscribe_read() {
    let local = LocalSet::new();
    local
        .run_until(async {
            // 1. A connected socket pair: one end runs the server RPC system locally,
            //    the other is handed to the client via UnixCapnpClient.
            let (server_side, client_side) = UnixStream::pair().unwrap();

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

            // 3. Stand up the client via UnixCapnpClient — the same path real apps take.
            let client_fd = client_side.into_std().unwrap().into_raw_fd();
            let unix =
                UnixCapnpClient::from_connected_fd(client_fd, rpc_twoparty_capnp::Side::Client)
                    .expect("connect");
            let client: CollabServiceClient = unix.bootstrap();
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
