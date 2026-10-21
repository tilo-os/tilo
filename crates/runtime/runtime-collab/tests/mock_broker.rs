//! Integration tests: end-to-end RPC over a bounded byte stream and Unix socket
//! against an in-memory `CollabService` server.
//!
//! Exercises the `UnixCapnpClient` plumbing through `runtime-portal`, the
//! generated `CollabService` schema, and the idiomatic
//! `CollabClient::join_channel` / `ChannelHandle::send` / `subscribe` /
//! `OpStream::read` wrappers.
//!
//! The broker is [`crate::fake_broker`]: any `channel_id` joins, `send`
//! records the op, and `subscribe` yields the seeded ops in order.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

#[cfg(unix)]
use std::os::fd::IntoRawFd;

use capnp_rpc::rpc_twoparty_capnp;
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::task::LocalSet;

use crate::fake_broker::{Captures, FakeBroker, serve};
use runtime_collab::{CollabClient, CollabServiceClient};
use runtime_portal::CapnpClient;

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

    let nonce = tilo_clock::try_unix_now_ns().unwrap();
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
            let captures = std::rc::Rc::new(std::cell::RefCell::new(Captures::default()));
            serve(
                FakeBroker {
                    captures: captures.clone(),
                    ..Default::default()
                },
                server_side,
            );

            let deadline = tilo_test_env::hang_bound();
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

            let captures = captures.borrow();
            assert_eq!(
                captures.joins,
                vec![
                    ("room".to_owned(), b"first".to_vec()),
                    ("record".to_owned(), b"second".to_vec()),
                    ("later".to_owned(), b"third".to_vec()),
                ]
            );
            assert_eq!(
                captures.sent_ops,
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
            let captures = std::rc::Rc::new(std::cell::RefCell::new(Captures::default()));
            serve(
                FakeBroker {
                    captures: captures.clone(),
                    op_backlog: vec![
                        (b"first-op".to_vec(), "fake-broker".to_owned()),
                        (b"second-op".to_vec(), "fake-broker".to_owned()),
                    ],
                    ..Default::default()
                },
                server_side,
            );

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
            let captures = captures.borrow();
            assert_eq!(
                captures.joins,
                vec![("test-channel".to_owned(), identity_blob)]
            );
            assert_eq!(
                captures.sent_ops,
                vec![b"hello".to_vec(), b"world".to_vec()]
            );
        })
        .await;
}
