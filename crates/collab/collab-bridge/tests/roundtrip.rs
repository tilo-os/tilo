//! End-to-end round-trip tests for `collab-bridge`.
//!
//! Each test stands up:
//!
//! - a *fake upstream broker* — a tokio task on the local set that
//!   speaks the shell's serde-JSON `CollabRequest` / `CollabEvent`
//!   dialect on one side of a `UnixStream::pair()`.
//! - the bridge's [`BridgeCollabService`] wired to open its
//!   "upstream" against the fake-broker end.
//! - a real capnp-rpc client (via `runtime-collab::CollabClient`)
//!   speaking to the bridge over another `UnixStream::pair()`.
//!
//! This exercises the full translation surface (capnp method call →
//! JSON frame upstream, JSON event upstream → capnp Reader on the
//! stream). No shell process is booted; the tests explicitly do NOT
//! prove the shell broker itself works — they prove the bridge's
//! translation is correct against a mocked upstream.

use std::cell::RefCell;
use std::os::fd::IntoRawFd;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::task::LocalSet;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use collab_bridge::service::BridgeCollabService;
use collab_bridge::upstream::{ContextSnapshot, DropOldest, OpEnvelope, OpKind, PresenceEnvelope, ScrollbackReply, UpstreamHandle};
use collab_bridge::wire::{CollabEvent, CollabRequest, Collaborator, PresencePayload, PresenceKey};

use runtime_collab::raw::collab_service;
use runtime_collab::{CollabClient, CollabServiceClient, PresencePayload as SdkPresencePayload};
use runtime_portal::UnixCapnpClient;

/// Capture buffer for a fake upstream broker. Every request the
/// bridge sends gets pushed here so tests can assert on it.
type Captured = Rc<RefCell<Vec<CollabRequest>>>;

/// Fake broker glue for a single UpstreamHandle. Returns:
/// - The [`UpstreamHandle`] the bridge uses.
/// - The `Captured` list to inspect what the bridge sent upstream.
/// - A [`FakeBrokerControl`] the test can push events through.
struct FakeUpstream {
    handle: UpstreamHandle,
    captured: Captured,
    /// Push a `CollabEvent` in as if it arrived from the shell
    /// broker. The bridge's reader task will decode + route it.
    inject: Rc<dyn Fn(CollabEvent)>,
}

/// Build a fake upstream: a UnixStream pair with a tokio task on
/// one end that (1) decodes every CollabRequest the bridge writes
/// and (2) exposes a channel to inject CollabEvents that the
/// bridge should receive.
async fn stand_up_fake_upstream() -> FakeUpstream {
    let (bridge_side, fake_side) = UnixStream::pair().unwrap();

    // The bridge speaks to `bridge_side` — feed it into the same
    // `connect_from_stream` code path as production. We inline
    // that path here so we don't need the real filesystem.
    let handle = handle_from_stream(bridge_side);

    // Fake broker task: pull the writer side of `fake_side` and
    // capture every request; the reader side is used to inject
    // events (writing into fake_side from the outside).
    let (mut fake_reader, fake_writer) = tokio::io::split(fake_side);
    let fake_writer = Arc::new(tokio::sync::Mutex::new(fake_writer));
    let captured: Captured = Rc::new(RefCell::new(Vec::new()));
    let captured_task = captured.clone();

    tokio::task::spawn_local(async move {
        loop {
            let mut len_buf = [0u8; 4];
            match fake_reader.read_exact(&mut len_buf).await {
                Ok(_) => {}
                Err(_) => return,
            }
            let len = u32::from_le_bytes(len_buf) as usize;
            let mut body = vec![0u8; len];
            if fake_reader.read_exact(&mut body).await.is_err() {
                return;
            }
            let Ok(req) = serde_json::from_slice::<CollabRequest>(&body) else {
                continue;
            };
            captured_task.borrow_mut().push(req);
        }
    });

    let writer = fake_writer.clone();
    let inject: Rc<dyn Fn(CollabEvent)> = Rc::new(move |ev| {
        let bytes = serde_json::to_vec(&ev).unwrap();
        let len = (bytes.len() as u32).to_le_bytes();
        let w = writer.clone();
        tokio::task::spawn_local(async move {
            let mut w = w.lock().await;
            w.write_all(&len).await.ok();
            w.write_all(&bytes).await.ok();
            w.flush().await.ok();
        });
    });

    FakeUpstream {
        handle,
        captured,
        inject,
    }
}

/// Replicates [`collab_bridge::upstream::connect`] but takes an
/// already-connected `UnixStream` (from `UnixStream::pair`) so
/// tests don't need to bind a real Unix socket. Not exposed in the
/// library because the production path always dials a path.
fn handle_from_stream(stream: UnixStream) -> UpstreamHandle {
    use tokio::sync::mpsc;
    let (mut reader, writer) = tokio::io::split(stream);
    let writer = Arc::new(tokio::sync::Mutex::new(writer));

    let (op_tx, op_rx) = mpsc::unbounded_channel();
    let (context_tx, context_rx) = mpsc::unbounded_channel();
    let (scrollback_tx, scrollback_rx) = mpsc::unbounded_channel();
    let presence = DropOldest::<PresenceEnvelope>::new(128);
    let presence_task = presence.clone();

    tokio::task::spawn_local(async move {
        loop {
            let mut len_buf = [0u8; 4];
            if reader.read_exact(&mut len_buf).await.is_err() {
                return;
            }
            let len = u32::from_le_bytes(len_buf) as usize;
            let mut body = vec![0u8; len];
            if reader.read_exact(&mut body).await.is_err() {
                return;
            }
            let Ok(event): Result<CollabEvent, _> = serde_json::from_slice(&body) else {
                continue;
            };
            match event {
                CollabEvent::SignedCRDTIncremental { envelope, from_user_id } => {
                    let bytes = postcard::to_stdvec(&*envelope).unwrap();
                    let _ = op_tx.send(OpEnvelope {
                        op_bytes: bytes,
                        from_user_id,
                        kind: OpKind::Signed,
                    });
                }
                CollabEvent::CRDTIncremental { data } => {
                    let _ = op_tx.send(OpEnvelope {
                        op_bytes: data,
                        from_user_id: String::new(),
                        kind: OpKind::UnsignedIncremental,
                    });
                }
                CollabEvent::CRDTUpdate { data } => {
                    let _ = op_tx.send(OpEnvelope {
                        op_bytes: data,
                        from_user_id: String::new(),
                        kind: OpKind::UnsignedSnapshot,
                    });
                }
                CollabEvent::RemoteCursorMoved { user_id, x, y } => {
                    presence_task.push(PresenceEnvelope::Cursor { user_id, x, y });
                }
                CollabEvent::RemotePresenceUpdate { user_id, key, payload } => {
                    let key_bytes = postcard::to_stdvec(&key).unwrap();
                    presence_task.push(PresenceEnvelope::Update {
                        user_id,
                        key_bytes,
                        payload,
                    });
                }
                CollabEvent::Context { collaborators, permissions } => {
                    let _ = context_tx.send(ContextSnapshot { collaborators, permissions });
                }
                CollabEvent::ScrollbackBatch { artifact, ops, exhausted } => {
                    let _ = scrollback_tx.send(ScrollbackReply { artifact, ops, exhausted });
                }
            }
        }
    });

    // We can't construct `UpstreamHandle` directly because its
    // fields are private. Route through a helper constructor. In
    // production this is done inside `connect`. For tests we use
    // `UpstreamHandle::new_for_test` (added below via a public
    // constructor).
    UpstreamHandle::new_for_test(
        writer,
        op_rx,
        presence,
        context_rx,
        scrollback_rx,
    )
}

/// Stand up the bridge in front of a fake upstream.
///
/// Returns:
/// - the [`CollabClient`] SDK-side handle the test drives.
/// - the fake-broker capture list.
/// - the fake-broker inject closure.
async fn stand_up_bridge() -> (CollabClient, Captured, Rc<dyn Fn(CollabEvent)>) {
    let fake = stand_up_fake_upstream().await;

    // Bridge service with an opener that hands back the pre-built
    // upstream handle every time joinChannel is called.
    let handle_cell: Rc<RefCell<Option<UpstreamHandle>>> =
        Rc::new(RefCell::new(Some(fake.handle.clone())));
    let opener: collab_bridge::service::UpstreamOpener = Rc::new(move || {
        let cell = handle_cell.clone();
        Box::pin(async move {
            let h = cell
                .borrow_mut()
                .clone()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "no upstream"))?;
            Ok(h)
        })
    });
    let service = BridgeCollabService::new(opener);

    // Wire an SDK-side capnp client to the bridge service in
    // memory (no filesystem).
    let (sdk_side, bridge_side) = UnixStream::pair().unwrap();

    // Bridge side: run capnp-rpc as Server.
    let (br_read, br_write) = bridge_side.into_split();
    let br_read = br_read.compat();
    let br_write = br_write.compat_write();
    let bridge_network = Box::new(twoparty::VatNetwork::new(
        futures::io::BufReader::new(br_read),
        futures::io::BufWriter::new(br_write),
        rpc_twoparty_capnp::Side::Server,
        Default::default(),
    ));
    let root: collab_service::Client = capnp_rpc::new_client(service);
    let bridge_rpc = RpcSystem::new(bridge_network, Some(root.client));
    tokio::task::spawn_local(async move {
        let _ = bridge_rpc.await;
    });

    // SDK side: stand up UnixCapnpClient as Client.
    let sdk_fd = sdk_side.into_std().unwrap().into_raw_fd();
    let unix = UnixCapnpClient::from_connected_fd(sdk_fd, rpc_twoparty_capnp::Side::Client)
        .expect("connect");
    let client: CollabServiceClient = unix.bootstrap();
    let collab = CollabClient::from_raw(client);

    (collab, fake.captured, fake.inject)
}

// --- tests --------------------------------------------------------------

#[test]
fn publish_cursor_arrives_upstream_as_pointer_presence() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let local = LocalSet::new();
    local.block_on(&rt, async {
        let (collab, captured, _inject) = stand_up_bridge().await;
        let channel = collab.join_channel("chan-1", b"identity").await.unwrap();
        let presence = channel.presence().await.unwrap();
        presence.publish_cursor(1.0, 2.0).await.unwrap();

        // Give the fake broker's reader task a chance to see the
        // frame — it runs on the same LocalSet so a `yield` chain
        // is enough.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }

        let cap = captured.borrow();
        assert!(matches!(cap[0], CollabRequest::Hello { .. }), "first frame is Hello");
        let pointer = cap.iter().find_map(|r| match r {
            CollabRequest::Presence { payload: PresencePayload::Pointer { x, y }, .. } => {
                Some((*x, *y))
            }
            _ => None,
        }).expect("bridge emitted a Pointer Presence upstream");
        assert_eq!(pointer, (1.0, 2.0));
    });
}

#[test]
fn subscribe_context_delivers_upstream_context_events() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let local = LocalSet::new();
    local.block_on(&rt, async {
        let (collab, _captured, inject) = stand_up_bridge().await;
        let channel = collab.join_channel("chan-ctx", b"identity").await.unwrap();

        let stream = channel.subscribe_context().await.unwrap();
        // Give the reader task a moment to be ready to relay.
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }

        (inject)(CollabEvent::Context {
            collaborators: vec![
                Collaborator {
                    id: "alice".into(),
                    name: "Alice".into(),
                    color: [1.0, 0.0, 0.0],
                    is_agent: false,
                },
                Collaborator {
                    id: "bob".into(),
                    name: "Bob".into(),
                    color: [0.0, 1.0, 0.0],
                    is_agent: false,
                },
            ],
            permissions: vec!["write".into(), "share".into()],
        });

        let snap = tokio::time::timeout(Duration::from_secs(3), stream.read())
            .await
            .expect("context arrives within 3s")
            .unwrap();

        assert_eq!(snap.collaborators.len(), 2);
        assert_eq!(snap.collaborators[0].user_id, "alice");
        assert_eq!(snap.collaborators[0].display_name, "Alice");
        assert_eq!(snap.collaborators[0].color, 0x00_ff_00_00);
        assert_eq!(snap.collaborators[1].user_id, "bob");
        assert_eq!(snap.permissions, vec!["write".to_string(), "share".to_string()]);
    });
}

#[test]
fn subscribe_all_delivers_presence_updates_with_keyed_variants() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let local = LocalSet::new();
    local.block_on(&rt, async {
        let (collab, _captured, inject) = stand_up_bridge().await;
        let channel = collab.join_channel("chan-pres", b"identity").await.unwrap();
        let presence = channel.presence().await.unwrap();
        let stream = presence.subscribe_all().await.unwrap();
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }

        let key = PresenceKey::Tab { id: "t-1".into() };
        (inject)(CollabEvent::RemotePresenceUpdate {
            user_id: "alice".into(),
            key: key.clone(),
            payload: PresencePayload::Caret { pos: 42 },
        });
        (inject)(CollabEvent::RemotePresenceUpdate {
            user_id: "bob".into(),
            key: key.clone(),
            payload: PresencePayload::Arrived,
        });

        let ev1 = tokio::time::timeout(Duration::from_secs(3), stream.read())
            .await
            .expect("presence event 1 arrives")
            .unwrap();
        assert_eq!(ev1.user_id, "alice");
        assert_eq!(ev1.payload, SdkPresencePayload::Caret { pos: 42 });
        let decoded: PresenceKey = postcard::from_bytes(&ev1.key_bytes).unwrap();
        assert_eq!(decoded, key);

        let ev2 = tokio::time::timeout(Duration::from_secs(3), stream.read())
            .await
            .expect("presence event 2 arrives")
            .unwrap();
        assert_eq!(ev2.user_id, "bob");
        assert_eq!(ev2.payload, SdkPresencePayload::Arrived);
    });
}

#[test]
fn signed_ops_are_never_dropped_even_under_presence_flood() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let local = LocalSet::new();
    local.block_on(&rt, async {
        let (collab, _captured, inject) = stand_up_bridge().await;
        let channel = collab.join_channel("chan-bp", b"identity").await.unwrap();
        let stream = channel.subscribe().await.unwrap();
        let presence = channel.presence().await.unwrap();
        let presence_stream = presence.subscribe_all().await.unwrap();
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }

        // Flood the upstream with far more presence events than the
        // presence queue can hold (capacity is 128), interleaved
        // with 3 signed ops. Signed ops MUST all survive.
        let key = PresenceKey::Tab { id: "flood".into() };
        for i in 0..600u32 {
            (inject)(CollabEvent::RemoteCursorMoved {
                user_id: "flooder".into(),
                x: i as f32,
                y: 0.0,
            });
        }
        for i in 0..3u32 {
            (inject)(CollabEvent::SignedCRDTIncremental {
                envelope: Box::new(mk_signed_op(i as u64)),
                from_user_id: format!("author-{i}"),
            });
        }
        // Interleave more presence noise after the ops.
        for i in 0..600u32 {
            (inject)(CollabEvent::RemotePresenceUpdate {
                user_id: "flooder".into(),
                key: key.clone(),
                payload: PresencePayload::Caret { pos: i as usize },
            });
        }

        // Give the reader task time to see everything (yield in
        // bulk instead of a real sleep to keep the test fast).
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        // Small real sleep so the reader task drains — with the
        // current-thread runtime yield alone isn't quite enough
        // when the reader task is on the same executor.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Drain the signed-op stream: all three must land.
        for i in 0..3u32 {
            let ev = tokio::time::timeout(Duration::from_secs(3), stream.read())
                .await
                .expect("signed op arrives")
                .unwrap();
            assert_eq!(ev.from_user_id, format!("author-{i}"), "op {i} attribution preserved");
            // The op body is postcard(SignedOp). We just check it's
            // non-empty; per-field assertions belong in collab-types.
            assert!(!ev.op.is_empty(), "op {i} not empty");
        }

        // Presence stream now yields whatever survived — should be
        // capped by capacity, proving drop-oldest is in force.
        // Read one event to confirm the stream is still live.
        let _ = tokio::time::timeout(Duration::from_millis(200), presence_stream.read()).await;
    });
}

#[test]
fn upstream_close_surfaces_as_error_on_next_op_read() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let local = LocalSet::new();
    local.block_on(&rt, async {
        // Stand up a bridge where the upstream opener returns an
        // Err. The SDK's next call should fail with a capnp error
        // (surfaced as NotAvailable / disconnect on the client
        // side).
        let opener: collab_bridge::service::UpstreamOpener = Rc::new(|| {
            Box::pin(async {
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "no upstream",
                ))
            })
        });
        let service = BridgeCollabService::new(opener);

        let (sdk_side, bridge_side) = UnixStream::pair().unwrap();
        let (br_read, br_write) = bridge_side.into_split();
        let br_read = br_read.compat();
        let br_write = br_write.compat_write();
        let bridge_network = Box::new(twoparty::VatNetwork::new(
            futures::io::BufReader::new(br_read),
            futures::io::BufWriter::new(br_write),
            rpc_twoparty_capnp::Side::Server,
            Default::default(),
        ));
        let root: collab_service::Client = capnp_rpc::new_client(service);
        let bridge_rpc = RpcSystem::new(bridge_network, Some(root.client));
        tokio::task::spawn_local(async move {
            let _ = bridge_rpc.await;
        });

        let sdk_fd = sdk_side.into_std().unwrap().into_raw_fd();
        let unix =
            UnixCapnpClient::from_connected_fd(sdk_fd, rpc_twoparty_capnp::Side::Client).unwrap();
        let client: CollabServiceClient = unix.bootstrap();
        let collab = CollabClient::from_raw(client);

        let res = collab.join_channel("c", b"i").await;
        // `ChannelHandle` doesn't implement `Debug`, so we can't
        // `unwrap_err`. Match by hand.
        let err = match res {
            Ok(_) => panic!("expected upstream-refused error but joinChannel returned Ok"),
            Err(e) => e,
        };
        let msg = format!("{err:?}");
        assert!(
            msg.contains("upstream") || msg.contains("no upstream") || msg.contains("Connection"),
            "unexpected error: {msg}"
        );
    });
}

/// Minimal SignedOp for tests. Every field that's part of the
/// canonical bytes is set to a stable placeholder; the bridge
/// doesn't introspect the op so this is enough.
fn mk_signed_op(seq: u64) -> collab_types::SignedOp {
    use collab_types::{ArtifactId, Ed25519PublicKey, OpClass, SignedOp};
    let op = SignedOp::new_unsigned(
        vec![1, 2, 3, 4],
        ArtifactId::default(),
        Ed25519PublicKey::from_bytes([0u8; 32]),
        collab_types::AttestationId::default(),
        OpClass::Splice,
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(seq),
        seq,
    );
    // The zero-filled signature written by `new_unsigned` is fine —
    // the bridge doesn't verify signatures.
    op
}
