use super::*;

/// Remote actions: who may still act, and what acting costs.
mod remote_action_tests;
use tilo_relay_shell::ShellAuthority as _;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationList, UserIdentity, sign_device_cert,
};

pub(super) fn certified_device(
    root: &ed25519_dalek::SigningKey,
    user: [u8; 32],
    label: &str,
) -> (AuthenticatedPeer, ed25519_dalek::SigningKey) {
    let (key, device) = DeviceIdentity::generate(label, 1);
    let mut cert = DeviceCert {
        user_public_key: user,
        device_public_key: device.public_key,
        device_label: label.into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    // Exercise certificate signature and nonce proof, matching the relay
    // handshake rather than constructing unchecked AuthenticatedPeer data.
    let hello = crate::auth::Hello::new(cert, [2; 32]);
    let peer = crate::auth::accept_hello(
        &hello,
        &user,
        &crate::policy::TrustPolicy::AllowMatchingUser,
        &RevocationList::new(),
        unix_now_ns(),
    )
    .unwrap();
    let proof = crate::auth::build_hello_ack2(&key, &[1; 32]);
    crate::auth::verify_hello_ack2(&proof, &peer.device_public_key, &[1; 32]).unwrap();
    (peer, key)
}

// In an unoptimized build, polling this scenario through the persistence
// owner nests deeper than the default 2 MiB test-thread stack (it needs
// more than 4 MiB when it runs alone). Run it on its own thread with an
// explicit stack instead of depending on RUST_MIN_STACK.

async fn activation_failure_fences_original_sessions_and_retry(operation: &'static str) {
    use crate::store::test_support::replay_frames;
    use tilo_relay_shell::selection::fixtures::{
        admit_frames, producer_fixture, producer_frame, producer_metadata,
    };
    let meta = producer_metadata();
    let authority = tilo_relay_protocol::retention::RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap();
    let id = crate::room_workspace_id(authority.room()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.store.bind_shell_authority(authority).unwrap();
    // Both captured sessions are already ready on this durable old basis.
    for row in meta["baseline"].as_array().unwrap() {
        let raw = producer_frame(&meta, row, 1);
        let Envelope::Delta { delta, .. } = crate::transport::decode(&raw[4..]).unwrap() else {
            panic!()
        };
        let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
            crate::coedit::CoeditPayload::decode(&delta).unwrap()
        else {
            panic!()
        };
        runtime.store.retain_checkpoint(&channel, &raw).unwrap();
    }
    let store = runtime.store.clone();
    let (root, user) = UserIdentity::generate("activation fault recipients", 1);
    let mut peers = Vec::new();
    #[expect(
        clippy::collection_is_never_read,
        reason = "holds the receivers open so their senders stay connected"
    )]
    let mut ordinary_receivers = Vec::new();
    let mut disconnects = Vec::new();
    let mut frames = Vec::new();
    for label in ["original", "replaced"] {
        let (peer, _) = certified_device(&root, user.public_key, label);
        runtime.admit_device(peer.device_public_key).unwrap();
        let (tx, rx) = mpsc::channel(32);
        let (disconnect, disconnected) = mpsc::channel(1);
        let (state_tx, state_rx) = mpsc::channel(32);
        runtime.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            role: peer.role,
            tx,
            disconnect,
        });
        runtime.state_sessions.insert(
            peer.device_public_key,
            StateSession {
                peer: peer.clone(),
                tx: state_tx,
                live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                replay: None,
                lag: None,
            },
        );
        peers.push(peer);
        ordinary_receivers.push(rx);
        disconnects.push(disconnected);
        frames.push(state_rx);
    }
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let room_half = producer_frame(&meta, &meta["tail"][0], 2);
    enqueue_state(
        &workspace,
        crate::transport::decode(&room_half[4..]).unwrap(),
        None,
        crate::auth::PeerRole::Device,
        metrics.clone(),
    )
    .await
    .unwrap();
    let owner = crate::persistence::handle(&workspace).await;
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    let current = store.root().join("shell-selected-v1/CURRENT");
    let before = std::fs::read(&current).unwrap();
    let raw = producer_frame(&meta, &meta["tail"][1], 3);
    let fault = crate::store::test_barrier::fail(store.root(), operation);
    let (release, reached) = crate::store::test_barrier::install(store.root(), operation);
    enqueue_state(
        &workspace,
        crate::transport::decode(&raw[4..]).unwrap(),
        None,
        crate::auth::PeerRole::Device,
        metrics.clone(),
    )
    .await
    .unwrap();
    tilo_test_env::within(
        "selected activation never reached its fault barrier",
        reached,
    )
    .await
    .unwrap();
    assert_ne!(
        std::fs::read(&current).unwrap(),
        before,
        "injection must follow CURRENT switch"
    );
    // A newer session replaces one captured channel while the original job
    // is paused. Error fencing must never disconnect this replacement.
    let (new_tx, _new_rx) = mpsc::channel(32);
    let (new_disconnect, mut new_disconnected) = mpsc::channel(1);
    let (new_state_tx, mut new_frames) = mpsc::channel(32);
    {
        let peer = &peers[1];
        let mut runtime = workspace.lock().await;
        runtime.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            role: peer.role,
            tx: new_tx.clone(),
            disconnect: new_disconnect,
        });
        runtime.state_sessions.insert(
            peer.device_public_key,
            StateSession {
                peer: peer.clone(),
                tx: new_state_tx,
                live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                replay: None,
                lag: None,
            },
        );
    }
    drop(release);
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    drop(fault);
    // Retry identical bytes after the durable cut: suppression must not
    // leave an old ready recipient connected forever without activation.
    enqueue_state(
        &workspace,
        crate::transport::decode(&raw[4..]).unwrap(),
        None,
        crate::auth::PeerRole::Device,
        metrics,
    )
    .await
    .unwrap();
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    assert!(
        disconnects[0].try_recv().is_ok(),
        "post-cut {operation} error must fence captured recipient before exact retry suppresses it"
    );
    assert!(frames[0].try_recv().is_err());
    {
        let runtime = workspace.lock().await;
        assert!(
            !runtime
                .state_sessions
                .contains_key(&peers[0].device_public_key)
        );
        assert!(
            runtime.subscribers[&peers[1].device_public_key]
                .tx
                .same_channel(&new_tx)
        );
    }
    assert!(
        new_disconnected.try_recv().is_err(),
        "replacement channel must survive old job failure"
    );
    assert!(
        new_frames.try_recv().is_err(),
        "exact retry remains semantically suppressed"
    );
    // Reconnect captures the real durable basis after resumed GC, admitting
    // every frame and converging to the producer's exact app heads.
    let peer = admit_frames(
        &meta,
        &producer_fixture().join("baseline-peer.postcard"),
        replay_frames(&store),
    );
    for row in meta["rows"].as_array().unwrap() {
        assert_eq!(
            peer.document_heads(
                row["app_id"].as_str().unwrap(),
                row["record_id"].as_str().unwrap()
            )
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
            serde_json::from_value::<Vec<String>>(row["heads"].clone()).unwrap()
        );
    }
}

#[tokio::test]
async fn selected_activation_post_current_failure_fences_original_sessions_on_exact_retry() {
    activation_failure_fences_original_sessions_and_retry("shell-after-current").await;
}
#[tokio::test]
async fn selected_activation_cursor_open_failure_fences_original_sessions_on_exact_retry() {
    activation_failure_fences_original_sessions_and_retry("shell-publication-open").await;
}

#[tokio::test]
async fn selected_shell_activation_publishes_complete_ordered_pair_and_suppresses_delayed_checkpoint()
 {
    use tilo_relay_shell::selection::fixtures::{
        admit_frames, producer_fixture, producer_frame, producer_metadata,
    };
    let meta = producer_metadata();
    let authority = tilo_relay_protocol::retention::RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap();
    let id = crate::room_workspace_id(authority.room()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.store.bind_shell_authority(authority).unwrap();
    let (root, user) = UserIdentity::generate("activation recipient", 1);
    let (receiver, _) = certified_device(&root, user.public_key, "recipient");
    runtime.admit_device(receiver.device_public_key).unwrap();
    let (tx, _rx) = mpsc::channel(32);
    let (disconnect, _) = mpsc::channel(1);
    let (state_tx, mut state_rx) = mpsc::channel(32);
    runtime.add_subscriber(Subscriber {
        device_public_key: receiver.device_public_key,
        role: receiver.role,
        tx,
        disconnect,
    });
    runtime.state_sessions.insert(
        receiver.device_public_key,
        StateSession {
            peer: receiver,
            tx: state_tx,
            live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            replay: None,
            lag: None,
        },
    );
    let (slow, _) = certified_device(&root, user.public_key, "saturated recipient");
    runtime.admit_device(slow.device_public_key).unwrap();
    let (slow_tx, _slow_rx) = mpsc::channel(1);
    let (slow_disconnect, mut slow_disconnected) = mpsc::channel(1);
    let (slow_state, mut slow_frames) = mpsc::channel(1);
    runtime.add_subscriber(Subscriber {
        device_public_key: slow.device_public_key,
        role: slow.role,
        tx: slow_tx,
        disconnect: slow_disconnect,
    });
    runtime.state_sessions.insert(
        slow.device_public_key,
        StateSession {
            peer: slow,
            tx: slow_state,
            live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            replay: None,
            lag: None,
        },
    );
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let mut delivered_all = Vec::new();
    for (group, start, end, expected) in [
        ("baseline", 0, 4, 4),
        ("tail", 0, 2, 2),
        ("tail", 2, 3, 1),
        ("checkpoint2", 0, 4, 4),
    ] {
        for row in &meta[group].as_array().unwrap()[start..end] {
            let raw = producer_frame(&meta, row, 10);
            enqueue_state(
                &workspace,
                crate::transport::decode(&raw[4..]).unwrap(),
                None,
                crate::auth::PeerRole::Device,
                metrics.clone(),
            )
            .await
            .unwrap();
        }
        let owner = crate::persistence::handle(&workspace).await;
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        let mut frames = Vec::new();
        while let Ok(delivery) = state_rx.try_recv() {
            frames.push((*delivery.envelope).clone());
        }
        assert_eq!(
            frames.len(),
            expected,
            "activation preserves exact selective live output for {group}[{start}..{end}]"
        );
        delivered_all.extend(frames);
        admit_frames(
            &meta,
            &producer_fixture().join("baseline-peer.postcard"),
            delivered_all.clone(),
        );
    }
    assert!(
        slow_disconnected.try_recv().is_ok(),
        "bounded activation queue saturation fences partial recipient"
    );
    assert!(slow_frames.try_recv().is_ok());
    assert!(slow_frames.try_recv().is_err());
    for row in meta["baseline"].as_array().unwrap() {
        let raw = producer_frame(&meta, row, u64::MAX - 1);
        enqueue_state(
            &workspace,
            crate::transport::decode(&raw[4..]).unwrap(),
            None,
            crate::auth::PeerRole::Device,
            metrics.clone(),
        )
        .await
        .unwrap();
    }
    let owner = crate::persistence::handle(&workspace).await;
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    assert!(
        state_rx.try_recv().is_err(),
        "delayed CP1 must not invalidate a ready CP2 receiver"
    );
}

#[tokio::test]
async fn shared_retained_metrics_count_only_newly_written_payload_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        7,
        false,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let envelope = Envelope::Delta {
        workspace_id: 7,
        author_device_public_key: [1; 32],
        authored_at_ns: 1,
        signature: [0; 64],
        delta: crate::coedit::CoeditPayload::Checkpoint {
            channel: [2; 32],
            body: vec![3; 64],
        }
        .encode()
        .unwrap(),
    };
    let expected = crate::transport::encode(&envelope).unwrap().len() as u64;
    for _ in 0..2 {
        enqueue_state(
            &workspace,
            envelope.clone(),
            None,
            crate::auth::PeerRole::Device,
            metrics.clone(),
        )
        .await
        .unwrap();
    }
    let owner = crate::persistence::handle(&workspace).await;
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    assert_eq!(
        metrics.snapshot().bytes_queued,
        expected,
        "checkpoint retry was counted as another payload write"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paused_state_persistence_releases_routing_lock() {
    paused_state_private_roundtrip(true, false, false, false, None).await;
}

/// A peer that pipelines more private requests than the reply queue holds,
/// while its own replies are not yet read, is backpressured: the relay
/// stops reading until the writer drains, and never disconnects it.
#[tokio::test(start_paused = true)]
async fn pipelined_private_requests_backpressure_instead_of_disconnecting() {
    const REQUESTS: usize = PEER_OUTBOUND_CAPACITY * 4;
    let (root, user) = UserIdentity::generate("alice", 1);
    let (a, _) = certified_device(&root, user.public_key, "a");
    let room = uuid::Uuid::from_u128(7);
    let id = crate::room_workspace_id(&room.to_string()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.admit_device(a.device_public_key).unwrap();
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(
        id,
        Arc::new(AsyncMutex::new(runtime)),
    )])));
    let ctx = PeerContext {
        workspaces: registry,
        autobind: None,
        revocations: Arc::new(std::sync::RwLock::new(RevocationList::new())),
        trust_policy: Arc::new(std::sync::RwLock::new(
            crate::policy::TrustPolicy::AllowMatchingUser,
        )),
        own_user_public_key: user.public_key,
        own_device_cert: a.cert.clone(),
        own_device_public_key: a.cert.device_public_key,
        app_serving: None,
        metrics: Arc::new(crate::metrics::Metrics::default()),
        live_bridge: None,
        agents: crate::peer::AgentRegistry::default(),
        positions: None,
        compactor: None,
        member_limits: None,
    };
    // Requests flow through a roomy pipe; replies through a small one, so
    // the relay's writer blocks until the client reads.
    let (mut requests, server_read) = tokio::io::duplex(1 << 20);
    let (server_write, replies) = tokio::io::duplex(256);
    let (server_read, _) = tokio::io::split(server_read);
    let (_, server_write) = tokio::io::split(server_write);
    let (replies, _) = tokio::io::split(replies);
    let relay = tokio::spawn(run_peer_stream(
        a.clone(),
        server_read,
        server_write,
        ctx,
        std::future::pending::<()>(),
    ));
    let mut reader = SessionReader::new(replies);
    requests
        .write_all(
            &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Subscribe {
                workspace_id: id,
            })
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        reader.read_next().await.unwrap(),
        Some(EnvelopeV2::State(Envelope::Ready { .. }))
    ));
    let enable =
        tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::EnableActions { room }).unwrap();
    for _ in 0..REQUESTS {
        requests.write_all(&enable).await.unwrap();
    }
    // The clock is paused: this returns only once the relay has nothing
    // left to do, its writer blocked on the unread replies.
    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    assert!(
        !relay.is_finished(),
        "the relay dropped a backpressured peer"
    );
    for _ in 0..REQUESTS {
        assert!(matches!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::ActionsReady { .. })
        ));
    }
    assert!(!relay.is_finished());
}

/// A member removed while its session's writer is held behind unread
/// frames, and whose session then ends, is still told: what was queued
/// when the session ended, the refusal among it, is written before the
/// stream closes.
#[tokio::test(start_paused = true)]
async fn a_refusal_queued_as_a_session_ends_reaches_the_peer() {
    refusal_queued_as_the_session_ends(16).await;
}

/// The same with the writer idle when the session ends: it is told, writes
/// the refusal and closes.
#[tokio::test(start_paused = true)]
async fn an_idle_writer_writes_the_refusal_and_closes_as_the_session_ends() {
    refusal_queued_as_the_session_ends(0).await;
}

/// Remove the member of a session whose writer `held` unread replies
/// hold, then end the session from the peer's side.
async fn refusal_queued_as_the_session_ends(held: usize) {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (a, _) = certified_device(&root, user.public_key, "a");
    let room = uuid::Uuid::from_u128(7);
    let id = crate::room_workspace_id(&room.to_string()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.admit_device(a.device_public_key).unwrap();
    let runtime = Arc::new(AsyncMutex::new(runtime));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, Arc::clone(&runtime))])));
    let ctx = PeerContext {
        workspaces: registry,
        autobind: None,
        revocations: Arc::new(std::sync::RwLock::new(RevocationList::new())),
        trust_policy: Arc::new(std::sync::RwLock::new(
            crate::policy::TrustPolicy::AllowMatchingUser,
        )),
        own_user_public_key: user.public_key,
        own_device_cert: a.cert.clone(),
        own_device_public_key: a.cert.device_public_key,
        app_serving: None,
        metrics: Arc::new(crate::metrics::Metrics::default()),
        live_bridge: None,
        agents: crate::peer::AgentRegistry::default(),
        positions: None,
        compactor: None,
        member_limits: None,
    };
    // Replies go through a pipe smaller than what is asked of the relay, so
    // its writer is held mid-write until the client reads.
    let (mut requests, server_read) = tokio::io::duplex(1 << 20);
    let (server_write, replies) = tokio::io::duplex(64);
    let (server_read, _) = tokio::io::split(server_read);
    let (_, server_write) = tokio::io::split(server_write);
    let (replies, _) = tokio::io::split(replies);
    let relay = tokio::spawn(run_peer_stream(
        a.clone(),
        server_read,
        server_write,
        ctx,
        std::future::pending::<()>(),
    ));
    let mut reader = SessionReader::new(replies);
    requests
        .write_all(
            &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Subscribe {
                workspace_id: id,
            })
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        reader.read_next().await.unwrap(),
        Some(EnvelopeV2::State(Envelope::Ready { .. }))
    ));
    let enable =
        tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::EnableActions { room }).unwrap();
    for _ in 0..held {
        requests.write_all(&enable).await.unwrap();
    }
    // The clock is paused: each sleep returns once the relay has nothing
    // left to do but wait for the client to read.
    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    runtime
        .lock()
        .await
        .refuse_removed_live_members(&[a.device_public_key]);
    drop(requests);
    tokio::time::sleep(std::time::Duration::from_secs(60)).await;

    let mut told = false;
    while let Some(frame) = reader.read_next().await.unwrap() {
        told |= matches!(
            frame,
            EnvelopeV2::State(Envelope::NotAuthorised { workspace_id, ref reason })
                if workspace_id == id && reason == "workspace membership changed"
        );
    }
    assert!(told, "the removed member was never told");
    relay.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paused_state_persistence_allows_same_connection_private_roundtrip() {
    paused_state_private_roundtrip(false, false, false, false, None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paused_replay_cleanup_with_full_reply_queue_keeps_private_reader_and_writer_live() {
    paused_state_private_roundtrip(false, true, false, false, None).await;
}

#[tokio::test]
async fn full_control_backlog_backpressures_and_recovers_after_held_writer() {
    paused_state_private_roundtrip(false, true, true, false, None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overtaking_private_keeps_held_state_authorization_future_pollable() {
    paused_state_private_roundtrip(false, false, false, true, None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plain_subscription_recovers_retained_frame_after_live_budget_exhaustion() {
    paused_state_private_roundtrip(false, false, false, false, Some(LagBoundary::Live)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plain_subscription_discards_stale_lag_after_explicit_unsubscribe() {
    paused_state_private_roundtrip(false, false, false, false, Some(LagBoundary::Unsubscribed))
        .await;
}

/// A read-only state request whose reply queues on the state lane like any
/// other: a stand-in for replies waiting behind a held writer.
fn reply_filler(workspace_id: u64) -> Vec<u8> {
    tilo_delivery_protocol::transport_v2::encode_state(&Envelope::VoiceCalls { workspace_id })
        .unwrap()
}

enum LagBoundary {
    Live,
    Unsubscribed,
}

#[expect(
    clippy::fn_params_excessive_bools,
    reason = "each flag selects one independent fault in the shared scenario"
)]
async fn paused_state_private_roundtrip(
    check_lock: bool,
    held_cleanup: bool,
    full_backlog: bool,
    held_authorization: bool,
    plain_lag: Option<LagBoundary>,
) {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (a, _) = certified_device(&root, user.public_key, "a");
    let (b, _) = certified_device(&root, user.public_key, "b");
    let room = uuid::Uuid::from_u128(7);
    let id = crate::room_workspace_id(&room.to_string()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    for device in [a.device_public_key, b.device_public_key, [99; 32]] {
        runtime.admit_device(device).unwrap();
    }
    let store = runtime.store.clone();
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, workspace.clone())])));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let mut wires = Vec::new();
    let mut tasks = Vec::new();
    for peer in [&a, &b] {
        let (client, server) = tokio::io::duplex(65536);
        let (read, write) = tokio::io::split(server);
        let ctx = PeerContext {
            workspaces: registry.clone(),
            autobind: None,
            revocations: revocations.clone(),
            trust_policy: trust.clone(),
            own_user_public_key: user.public_key,
            own_device_cert: a.cert.clone(),
            own_device_public_key: a.cert.device_public_key,
            app_serving: None,
            metrics: Arc::new(crate::metrics::Metrics::default()),
            live_bridge: None,
            agents: crate::peer::AgentRegistry::default(),
            positions: None,
            compactor: None,
            member_limits: None,
        };
        tasks.push(tokio::spawn(run_peer_stream(
            peer.clone(),
            read,
            write,
            ctx,
            std::future::pending::<()>(),
        )));
        let (read, mut write) = tokio::io::split(client);
        let mut reader = SessionReader::new(read);
        write
            .write_all(
                &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Subscribe {
                    workspace_id: id,
                })
                .unwrap(),
            )
            .await
            .unwrap();
        assert!(matches!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(Envelope::Ready { .. }))
        ));
        write
            .write_all(
                &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::EnableActions { room })
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(matches!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::ActionsReady { .. })
        ));
        wires.push((reader, write));
    }
    let (mut ar, mut aw) = wires.remove(0);
    let (mut br, mut bw) = wires.remove(0);
    if let Some(boundary) = plain_lag {
        let lag = workspace.lock().await.state_sessions[&b.device_public_key]
            .lag
            .as_ref()
            .unwrap()
            .clone();
        let envelope = Envelope::Delta {
            workspace_id: id,
            author_device_public_key: a.device_public_key,
            authored_at_ns: 1,
            signature: [0; 64],
            delta: crate::coedit::CoeditPayload::RetainedDelta {
                channel: [3; 32],
                body: vec![4; 4096],
            }
            .encode()
            .unwrap(),
        };
        let fault = crate::store::test_barrier::fail(store.root(), "live-delivery-budget");
        let (publication_release, published) =
            crate::store::test_barrier::install(store.root(), "live-publication-complete");
        let (release, reached) =
            crate::store::test_barrier::install(store.root(), "live-delivery-budget");
        aw.write_all(&tilo_delivery_protocol::transport_v2::encode_state(&envelope).unwrap())
            .await
            .unwrap();
        tilo_test_env::within("the retained publication to reach its live budget", reached)
            .await
            .expect("ordinary retained publication never reached live budget");
        drop(release);
        tilo_test_env::within("the missed live publication recipient fan-out", published)
            .await
            .expect("missed live publication never completed its recipient fan-out");
        drop(publication_release);
        let owner = crate::persistence::handle(&workspace).await;
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        drop(fault);
        assert!(
            store
                .load_retained()
                .unwrap()
                .iter()
                .any(|bytes| { crate::transport::decode(&bytes[4..]).unwrap() == envelope }),
            "missed live frame must remain durably retained"
        );
        let unsubscribed = matches!(boundary, LagBoundary::Unsubscribed);
        if unsubscribed {
            bw.write_all(
                &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Unsubscribe {
                    workspace_id: id,
                })
                .unwrap(),
            )
            .await
            .unwrap();
            // A heartbeat is answered ahead of a control still running, so
            // the unsubscribe's own effect is what this waits for.
            tilo_test_env::within("the explicit unsubscribe to complete", async {
                while workspace
                    .lock()
                    .await
                    .state_sessions
                    .contains_key(&b.device_public_key)
                {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await;
            bw.write_all(
                &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Ping).unwrap(),
            )
            .await
            .unwrap();
            let closed =
                tilo_test_env::within("the session to answer after its unsubscribe", async {
                    loop {
                        match br.read_next().await.unwrap() {
                            Some(EnvelopeV2::State(Envelope::Pong)) => break false,
                            Some(EnvelopeV2::State(
                                Envelope::Delta { .. } | Envelope::Ready { .. },
                            )) => {}
                            None => break true,
                            other => panic!("unexpected explicit-unsubscribe reply: {other:?}"),
                        }
                    }
                })
                .await;
            if closed {
                // The relay ended the session: say how, not just that it did.
                let ended = tilo_test_env::within("the closed session's outcome", tasks.remove(1))
                    .await
                    .expect("the closed session's outcome");
                panic!("the relay closed the session on explicit unsubscribe: {ended:?}");
            }
            assert!(
                !workspace
                    .lock()
                    .await
                    .state_sessions
                    .contains_key(&b.device_public_key)
            );
            // A queued hint may outlive the subscription that produced it.
            // Its ordering is fixed here after the real unsubscribe reply.
            lag.mark(id);
        }
        // Publication has completed, so the lag marker was made. Wait for
        // its actual consumption rather than guessing the polling interval.
        tilo_test_env::within("the peer to consume its lag marker", async {
            while !lag.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        bw.write_all(&tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Ping).unwrap())
            .await
            .unwrap();
        let mut frames = Vec::new();
        let mut ready = 0_usize;
        // The heartbeat answers at once, possibly ahead of a catch-up still
        // replaying: a live subscription waits for its replay's Ready too.
        // An unsubscribed one starts no replay, so its Pong follows every
        // reply the lag hint could have produced.
        let mut answered = false;
        tilo_test_env::within("retained catch-up before the next peer reply", async {
            while !(answered && (unsubscribed || ready > 0)) {
                match br.read_next().await.unwrap() {
                    Some(EnvelopeV2::State(Envelope::Pong)) => answered = true,
                    Some(EnvelopeV2::State(Envelope::Ready { .. })) => {
                        ready = ready.checked_add(1).unwrap();
                    }
                    Some(EnvelopeV2::State(frame @ Envelope::Delta { .. })) => {
                        frames.push(frame);
                    }
                    other => panic!("unexpected lag-recovery frame: {other:?}"),
                }
            }
        })
        .await;
        if !unsubscribed {
            assert!(
                workspace.lock().await.state_sessions[&b.device_public_key]
                    .live
                    .load(std::sync::atomic::Ordering::SeqCst),
                "ordinary byte backpressure must preserve the authorized subscription"
            );
        }
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
        if unsubscribed {
            assert!(
                frames.is_empty(),
                "a stale lag hint reopened an explicitly closed subscription"
            );
            assert_eq!(
                ready, 0,
                "an explicitly closed subscription cannot become ready again"
            );
            assert!(
                !workspace
                    .lock()
                    .await
                    .state_sessions
                    .contains_key(&b.device_public_key)
            );
        } else {
            assert_eq!(
                frames,
                vec![envelope],
                "plain subscription lost its committed retained frame"
            );
            assert_eq!(
                ready, 1,
                "catch-up must complete its actual replay boundary"
            );
        }
        return;
    }
    let envelope = Envelope::Delta {
        workspace_id: id,
        author_device_public_key: a.device_public_key,
        authored_at_ns: 1,
        signature: [0; 64],
        delta: crate::coedit::CoeditPayload::Checkpoint {
            channel: [3; 32],
            body: vec![4; 4096],
        }
        .encode()
        .unwrap(),
    };
    let (release, reached) = crate::store::test_barrier::install(
        store.root(),
        if held_authorization {
            "membership-read"
        } else if held_cleanup {
            "replay-cleanup"
        } else {
            "state-persist"
        },
    );
    if held_cleanup {
        let pending = Envelope::Delta {
            workspace_id: id,
            author_device_public_key: b.device_public_key,
            authored_at_ns: 3,
            delta: vec![255; 64],
            signature: [0; 64],
        };
        store
            .enqueue_pending(
                &a.device_public_key,
                3,
                &crate::transport::encode(&pending).unwrap(),
            )
            .unwrap();
        aw.write_all(
            &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Subscribe {
                workspace_id: id,
            })
            .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            ar.read_next().await.unwrap(),
            Some(EnvelopeV2::State(pending))
        );
    } else {
        aw.write_all(&tilo_delivery_protocol::transport_v2::encode_state(&envelope).unwrap())
            .await
            .unwrap();
    }
    reached.await.unwrap();
    if held_cleanup {
        // Replies that queue behind the held replay until the reply queue,
        // then the reader's backlog, is full.
        for _ in 0..if full_backlog { 64 } else { 12 } {
            aw.write_all(&reply_filler(id)).await.unwrap();
        }
    }
    if full_backlog {
        // The writer is confirmed inside the held cleanup. No state reply
        // can drain yet; enough real input fills reply and control queues.
        let held =
            tokio::time::timeout(std::time::Duration::from_millis(100), ar.read_next()).await;
        drop(release);
        let owner = crate::persistence::handle(&workspace).await;
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        if held.is_ok() {
            let ended = tasks.remove(0).await.unwrap();
            panic!(
                "full control backlog disconnected a healthy held writer: {held:?}; server result: {ended:?}"
            );
        }
        assert!(matches!(
            ar.read_next().await.unwrap(),
            Some(EnvelopeV2::State(Envelope::Ready { .. }))
        ));
        for _ in 0..64 {
            assert!(matches!(
                ar.read_next().await.unwrap(),
                Some(EnvelopeV2::State(Envelope::VoiceCallList { .. }))
            ));
        }
        for task in tasks {
            task.abort();
        }
        return;
    }
    if held_cleanup {
        // Behind a full reply queue and a writer held in the replay's store
        // cleanup, as on a slow disk, a heartbeat is still answered.
        aw.write_all(&tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Ping).unwrap())
            .await
            .unwrap();
        assert_eq!(
            tilo_test_env::within("a heartbeat answered under the held replay", ar.read_next())
                .await
                .unwrap(),
            Some(EnvelopeV2::State(Envelope::Pong))
        );
    }
    assert!(
        store.load_retained().unwrap().is_empty(),
        "state published before commit"
    );
    let request = DirectedAction {
        room,
        request_id: uuid::Uuid::from_u128(8),
        source: a.device_public_key,
        target: b.device_public_key,
        message: ActionMessage::CatalogueRefresh { scope: None },
    };
    if held_authorization {
        // The state handler holds the authorization lane while its real
        // filesystem read is paused. Confirm the nested private handler
        // has entered before releasing the read: no timing-based race.
        let (private_release, private_reached) =
            crate::store::test_barrier::install(store.root(), "private-authorization");
        aw.write_all(
            &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::Directed(request.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
        // Both waits are events, not wall-clock budgets: a private handler
        // that cannot enter lookahead, or a state future that stops being
        // polled, hangs here and nextest's slow-timeout reports it.
        private_reached
            .await
            .expect("private handler never entered lookahead");
        drop(release);
        drop(private_release);
        let mut state_seen = false;
        let mut private_seen = false;
        while !state_seen || !private_seen {
            match br.read_next().await.unwrap() {
                Some(EnvelopeV2::State(actual)) => {
                    assert_eq!(actual, envelope);
                    assert!(!state_seen);
                    state_seen = true;
                }
                Some(EnvelopeV2::Delivered { action, .. }) => {
                    assert_eq!(action, request);
                    assert!(!private_seen);
                    private_seen = true;
                }
                other => panic!(
                    "overtaking private handler stopped polling the state future holding its authorization lane: {other:?}"
                ),
            }
        }
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
        return;
    }
    let progress = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        if check_lock {
            drop(workspace.lock().await);
        }
        let encoded_request =
            tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::Directed(request.clone()))
                .unwrap();
        if check_lock {
            aw.write_all(&encoded_request).await.unwrap();
        } else {
            // A partial header remains in the stateful reader while its
            // independent writer completes an unrelated private frame.
            aw.write_all(&encoded_request[..2]).await.unwrap();
            let probe = DirectedAction {
                request_id: uuid::Uuid::from_u128(9),
                source: b.device_public_key,
                target: a.device_public_key,
                ..request.clone()
            };
            bw.write_all(
                &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::Directed(probe.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
            let Some(EnvelopeV2::Delivered { action, .. }) = ar.read_next().await.unwrap() else {
                panic!("expected private frame during partial inbound header")
            };
            assert_eq!(action, probe);
            aw.write_all(&encoded_request[2..]).await.unwrap();
        }
        let Some(EnvelopeV2::Delivered { action, .. }) = br.read_next().await.unwrap() else {
            panic!("expected private request before any uncommitted state")
        };
        assert_eq!(action, request);
        let response = DirectedAction {
            source: b.device_public_key,
            target: a.device_public_key,
            ..request.clone()
        };
        bw.write_all(
            &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::Directed(response.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
        let Some(EnvelopeV2::Delivered { action, .. }) = ar.read_next().await.unwrap() else {
            panic!("expected private response")
        };
        assert_eq!(action, response);
    })
    .await;
    // Always release the real blocking disk operation before asserting RED.
    drop(release);
    let owner = crate::persistence::handle(&workspace).await;
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    for task in tasks {
        task.abort();
    }
    assert!(
        progress.is_ok(),
        "held state persistence blocked private request/response (routing-lock check: {check_lock})"
    );
}

#[tokio::test]
async fn capability_controls_fence_dependent_private_during_held_operation() {
    for control in 0..3 {
        capability_control_lookahead(false, control).await;
    }
}

#[tokio::test]
async fn capability_controls_fence_dependent_private_during_reply_wait() {
    for control in 0..3 {
        capability_control_lookahead(true, control).await;
    }
}

#[tokio::test]
async fn heartbeat_keeps_progress_during_unrelated_workspace_admission() {
    capability_control_lookahead(false, 3).await;
}

#[tokio::test]
async fn heartbeat_is_answered_ahead_of_queued_capability_control() {
    capability_control_lookahead(false, 4).await;
}

async fn capability_control_lookahead(reply_wait: bool, control: u8) {
    let (root, user) = UserIdentity::generate("owner", 1);
    let (a, _) = certified_device(&root, user.public_key, "a");
    let (b, _) = certified_device(&root, user.public_key, "b");
    let rooms = [
        uuid::Uuid::from_u128(71),
        uuid::Uuid::from_u128(72),
        uuid::Uuid::from_u128(73),
    ];
    let ids = rooms.map(|room| crate::room_workspace_id(&room.to_string()).unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let mut map = HashMap::new();
    for id in ids {
        let mut runtime = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            id,
            true,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        for peer in [&a, &b] {
            runtime.admit_device(peer.device_public_key).unwrap();
        }
        map.insert(id, Arc::new(AsyncMutex::new(runtime)));
    }
    let held_workspace = map.get(&ids[2]).unwrap().clone();
    let store = held_workspace.lock().await.store.clone();
    let registry = Arc::new(AsyncMutex::new(map));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let host = uuid::Uuid::from_u128(74);
    let mut tasks = Vec::new();
    let mut wires = Vec::new();
    for (index, peer) in [&a, &b].into_iter().enumerate() {
        let (client, server) = tokio::io::duplex(65536);
        let (read, write) = tokio::io::split(server);
        tasks.push(tokio::spawn(run_peer_stream(
            peer.clone(),
            read,
            write,
            PeerContext {
                workspaces: registry.clone(),
                autobind: None,
                revocations: revocations.clone(),
                trust_policy: trust.clone(),
                own_user_public_key: user.public_key,
                own_device_cert: a.cert.clone(),
                own_device_public_key: a.cert.device_public_key,
                app_serving: None,
                metrics: Arc::new(crate::metrics::Metrics::default()),
                live_bridge: None,
                agents: crate::peer::AgentRegistry::default(),
                positions: None,
                compactor: None,
                member_limits: None,
            },
            std::future::pending::<()>(),
        )));
        let (read, mut write) = tokio::io::split(client);
        let mut reader = SessionReader::new(read);
        for (position, room) in rooms.iter().enumerate() {
            write
                .write_all(
                    &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Subscribe {
                        workspace_id: ids[position],
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
            assert!(matches!(
                reader.read_next().await.unwrap(),
                Some(EnvelopeV2::State(Envelope::Ready { .. }))
            ));
            if position < 2 && !(index == 0 && position == 0 && control == 0) {
                write
                    .write_all(
                        &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::EnableActions {
                            room: *room,
                        })
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                assert!(matches!(
                    reader.read_next().await.unwrap(),
                    Some(EnvelopeV2::ActionsReady { .. })
                ));
            }
        }
        if index == 1 && control > 0 {
            write
                .write_all(
                    &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::RegisterBroker {
                        room: rooms[0],
                        host,
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
            assert!(matches!(
                reader.read_next().await.unwrap(),
                Some(EnvelopeV2::ActionsReady { .. })
            ));
        }
        wires.push((reader, write));
    }
    let (mut ar, mut aw) = wires.remove(0);
    let (mut br, mut bw) = wires.remove(0);
    if control >= 3 {
        let gated = control == 4;
        let (release, reached) =
            crate::store::test_barrier::install(store.root(), "membership-read");
        let envelope = Envelope::Delta {
            workspace_id: ids[2],
            author_device_public_key: a.device_public_key,
            authored_at_ns: 1,
            signature: [0; 64],
            delta: crate::coedit::CoeditPayload::RetainedDelta {
                channel: [3; 32],
                body: vec![4; 4096],
            }
            .encode()
            .unwrap(),
        };
        aw.write_all(&tilo_delivery_protocol::transport_v2::encode_state(&envelope).unwrap())
            .await
            .unwrap();
        tilo_test_env::within("the state admission membership read", reached)
            .await
            .unwrap();
        assert!(store.load_retained().unwrap().is_empty());
        let request = DirectedAction {
            room: rooms[1],
            request_id: uuid::Uuid::from_u128(77),
            source: a.device_public_key,
            target: b.device_public_key,
            message: ActionMessage::CatalogueRefresh { scope: None },
        };
        if gated {
            aw.write_all(
                &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::EnableActions {
                    room: rooms[0],
                })
                .unwrap(),
            )
            .await
            .unwrap();
        }
        for frame in [
            EnvelopeV2::State(Envelope::Ping),
            EnvelopeV2::Directed(request.clone()),
        ] {
            aw.write_all(&tilo_delivery_protocol::transport_v2::encode(&frame).unwrap())
                .await
                .unwrap();
        }
        let Some(EnvelopeV2::Delivered { action, .. }) =
            tilo_test_env::within("the unrelated-room private request", br.read_next())
                .await
                .unwrap()
        else {
            panic!("unrelated private request stalled during state admission")
        };
        assert_eq!(action, request);
        let response = DirectedAction {
            source: b.device_public_key,
            target: a.device_public_key,
            ..request
        };
        bw.write_all(
            &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::Directed(response.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
        // The heartbeat is answered at once, ahead of the held state
        // admission and of any capability control queued behind it; a real
        // later private response proves the reader and writer stay live.
        let first = tilo_test_env::within("the heartbeat", ar.read_next())
            .await
            .unwrap();
        let later =
            tilo_test_env::within("the private response after the heartbeat", ar.read_next())
                .await
                .unwrap();
        assert!(store.load_retained().unwrap().is_empty());
        drop(release);
        let owner = crate::persistence::handle(&held_workspace).await;
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        let delivered_state =
            tilo_test_env::within("the state delivery after admission release", br.read_next())
                .await
                .unwrap();
        assert_eq!(delivered_state, Some(EnvelopeV2::State(envelope)));
        let ready = if gated {
            tilo_test_env::within("the queued capability reply", ar.read_next())
                .await
                .unwrap()
        } else {
            None
        };
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
        assert_eq!(
            first,
            Some(EnvelopeV2::State(Envelope::Pong)),
            "a live transport withheld its heartbeat behind a held state admission or control"
        );
        assert!(
            matches!(later, Some(EnvelopeV2::Delivered { action, .. }) if action == response),
            "heartbeat progress lost the authorized private response"
        );
        if gated {
            assert!(
                matches!(ready, Some(EnvelopeV2::ActionsReady { room, .. }) if room == rooms[0]),
                "the capability control lost its reply: {ready:?}"
            );
        }
        return;
    }
    let (release, reached) = crate::store::test_barrier::install(
        store.root(),
        if reply_wait {
            "replay-cleanup"
        } else {
            "replay-prepare"
        },
    );
    if reply_wait {
        let pending = Envelope::Delta {
            workspace_id: ids[2],
            author_device_public_key: b.device_public_key,
            authored_at_ns: 1,
            delta: vec![255; 8],
            signature: [0; 64],
        };
        store
            .enqueue_pending(
                &a.device_public_key,
                1,
                &crate::transport::encode(&pending).unwrap(),
            )
            .unwrap();
    }
    aw.write_all(
        &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Subscribe {
            workspace_id: ids[2],
        })
        .unwrap(),
    )
    .await
    .unwrap();
    if reply_wait {
        assert!(matches!(
            ar.read_next().await.unwrap(),
            Some(EnvelopeV2::State(Envelope::Delta { .. }))
        ));
    }
    reached.await.unwrap();
    if reply_wait {
        // Replies that queue behind the held replay until the reply queue
        // is full: the reader then waits on the writer.
        for _ in 0..12 {
            aw.write_all(&reply_filler(ids[2])).await.unwrap();
        }
    }
    let capability = match control {
        0 => EnvelopeV2::EnableActions { room: rooms[0] },
        1 => EnvelopeV2::EnableBrokerClient { room: rooms[0] },
        _ => EnvelopeV2::RegisterBroker {
            room: rooms[0],
            host,
        },
    };
    let dependent = DirectedAction {
        room: rooms[0],
        request_id: uuid::Uuid::from_u128(75),
        source: a.device_public_key,
        target: b.device_public_key,
        message: ActionMessage::CatalogueRefresh { scope: None },
    };
    let unrelated = DirectedAction {
        room: rooms[1],
        request_id: uuid::Uuid::from_u128(76),
        ..dependent.clone()
    };
    for frame in [
        capability,
        EnvelopeV2::Directed(dependent.clone()),
        EnvelopeV2::Directed(unrelated.clone()),
    ] {
        aw.write_all(&tilo_delivery_protocol::transport_v2::encode(&frame).unwrap())
            .await
            .unwrap();
    }
    let progress = tokio::time::timeout(tilo_test_env::hang_bound(), br.read_next()).await;
    // Receiving this later unrelated frame proves the reader classified the
    // earlier capability and dependent frame while the real owner was held.
    let premature =
        tokio::time::timeout(std::time::Duration::from_millis(50), ar.read_next()).await;
    drop(release);
    let owner = crate::persistence::handle(&held_workspace).await;
    crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
        .await
        .unwrap();
    assert!(
        matches!(progress, Ok(Ok(Some(EnvelopeV2::Delivered { action, .. }))) if action == unrelated),
        "unrelated-room private traffic stalled"
    );
    assert!(
        premature.is_err(),
        "dependent private frame overtook capability {control} (reply wait={reply_wait}): {premature:?}"
    );
    let delivered = tilo_test_env::within("the next frame from br", br.read_next())
        .await
        .unwrap()
        .unwrap();
    if control > 0 {
        assert!(matches!(&delivered, EnvelopeV2::CertifiedDelivered { .. }));
    }
    match delivered {
        EnvelopeV2::Delivered { action, .. } | EnvelopeV2::CertifiedDelivered { action, .. } => {
            assert_eq!(action, dependent);
        }
        other => panic!("dependent request did not observe capability transition: {other:?}"),
    }
    for task in tasks {
        task.abort();
    }
}

#[tokio::test]
async fn retained_replay_failure_and_duplicate_retirement_are_recoverable() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (peer, _) = certified_device(&root, user.public_key, "reader");
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        7,
        false,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.admit_device(peer.device_public_key).unwrap();
    let envelope = |author, payload: crate::coedit::CoeditPayload| Envelope::Delta {
        workspace_id: 7,
        author_device_public_key: [author; 32],
        authored_at_ns: 1,
        delta: payload.encode().unwrap(),
        signature: [0; 64],
    };
    let cp = envelope(
        1,
        crate::coedit::CoeditPayload::Checkpoint {
            channel: [9; 32],
            body: vec![1],
        },
    );
    let tail = envelope(
        1,
        crate::coedit::CoeditPayload::RetainedDelta {
            channel: [9; 32],
            body: vec![2],
        },
    );
    let other = envelope(
        2,
        crate::coedit::CoeditPayload::Checkpoint {
            channel: [9; 32],
            body: vec![3],
        },
    );
    let cp_bytes = crate::transport::encode(&cp).unwrap();
    runtime
        .store
        .retain_checkpoint(&[9; 32], &cp_bytes)
        .unwrap();
    runtime
        .store
        .retain_delta(&[9; 32], 2, &crate::transport::encode(&tail).unwrap())
        .unwrap();
    runtime
        .store
        .enqueue_pending(&peer.device_public_key, 1, &cp_bytes)
        .unwrap();
    runtime
        .store
        .enqueue_pending(
            &peer.device_public_key,
            2,
            &crate::transport::encode(&other).unwrap(),
        )
        .unwrap();
    runtime
        .store
        .enqueue_pending(&peer.device_public_key, 4, &cp_bytes)
        .unwrap();
    let opaque = b"unknown pending bytes";
    runtime
        .store
        .enqueue_pending(&peer.device_public_key, 3, opaque)
        .unwrap();
    // Matching undecodable retained bytes do NOT constitute replay coverage.
    let lineage = runtime.store.root().join("retained/v2").join(format!(
        "{}-{}",
        hex::encode([1; 32]),
        hex::encode([9; 32])
    ));
    let active = std::fs::read_to_string(lineage.join("CURRENT")).unwrap();
    std::fs::write(lineage.join(active.trim()).join("opaque.delta"), opaque).unwrap();
    let shared = Arc::new(AsyncMutex::new(runtime));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(7, shared.clone())])));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let (outbound, _rx) = mpsc::channel(8);
    let (disconnect, _) = mpsc::channel(1);
    let (private, _) = mpsc::channel(8);
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &outbound,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &peer.cert,
        own_device_public_key: &peer.cert.device_public_key,
        app_serving: None,
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &disconnect,
        presence: None,
        private_tx: &private,
        revocations: &revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    let (mut broken, reader) = tokio::io::duplex(1);
    drop(reader);
    assert!(
        Box::pin(handle_inbound_frame(
            Envelope::Subscribe { workspace_id: 7 },
            &peer,
            &session,
            &mut Vec::new(),
            &mut broken
        ))
        .await
        .is_err()
    );
    assert_eq!(
        shared
            .lock()
            .await
            .store
            .count_pending(&peer.device_public_key)
            .unwrap(),
        4
    );
    // Finish the old owner's cancelled-ticket disposal before emulating a
    // new process. Two owners over the same store are not a daemon restart.
    tilo_test_env::within("old owner did not release cancelled replay", async {
        loop {
            if std::fs::read_dir(shared.lock().await.store.root().join("store-work-v1"))
                .unwrap()
                .next()
                .is_none()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    // Emulate a crash during resumable duplicate cleanup: write the complete
    // replay, remove only its first duplicate, then reopen before the rest
    // of the cleanup list can be processed.
    let store = shared.lock().await.store.clone();
    let mut capture = store
        .start_replay_capture(
            crate::store::replay::ReplayId(*uuid::Uuid::new_v4().as_bytes()),
            peer.device_public_key,
        )
        .unwrap();
    let mut snapshot = loop {
        if let Step::Done(snapshot) = store
            .capture_replay_step(&mut capture, WorkLimit::default())
            .unwrap()
        {
            break snapshot;
        }
    };
    while matches!(
        store
            .prepare_replay_step(&mut snapshot, WorkLimit::default())
            .unwrap(),
        Step::More
    ) {}
    let budget = crate::store::replay::ReplayBudget::new(1024 * 1024);
    loop {
        let frames = store
            .read_replay_batch(
                &mut snapshot,
                crate::store::replay::ReplayLimits::default(),
                &budget,
            )
            .unwrap();
        if frames.eof {
            break;
        }
    }
    store
        .cleanup_replay_step(
            &mut snapshot,
            crate::store::replay::WorkLimit {
                entries: 1,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        shared
            .lock()
            .await
            .store
            .count_pending(&peer.device_public_key)
            .unwrap(),
        3
    );
    drop(snapshot);
    drop(capture);
    // Reopen the actual disk state after the failed replay.
    *shared.lock().await = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        7,
        false,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    let mut wire = Vec::new();
    Box::pin(handle_inbound_frame(
        Envelope::Subscribe { workspace_id: 7 },
        &peer,
        &session,
        &mut Vec::new(),
        &mut wire,
    ))
    .await
    .unwrap();
    let mut decoded = Vec::new();
    let mut reader = SessionReader::new(wire.as_slice());
    while let Some(frame) = reader.read_next().await.unwrap() {
        let EnvelopeV2::State(envelope) = frame else {
            panic!("a replay carried {frame:?}");
        };
        decoded.push(envelope);
    }
    assert_eq!(
        decoded,
        vec![
            cp,
            tail,
            other,
            Envelope::Ready {
                workspace_id: 7,
                pending_deltas: 3
            }
        ]
    );
    let pending = shared
        .lock()
        .await
        .store
        .load_pending(&peer.device_public_key)
        .unwrap();
    assert_eq!(
        pending.len(),
        1,
        "unknown pending entry was incorrectly retired"
    );
    assert_eq!(pending[0].bytes, opaque);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_membership_read_releases_routing_lock_and_fences_replaced_session() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (a, _a_key) = certified_device(&root, user.public_key, "a");
    let (b, _) = certified_device(&root, user.public_key, "b");
    let room = uuid::Uuid::from_u128(7);
    let id = crate::room_workspace_id(&room.to_string()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut wr = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    wr.admit_device(a.device_public_key).unwrap();
    wr.admit_device(b.device_public_key).unwrap();
    let (atx, _arx) = mpsc::channel(2);
    let (btx, _brx) = mpsc::channel(2);
    let (ap, _apr) = mpsc::channel(2);
    let (bp, mut bpr) = mpsc::channel(1);
    let (disconnect, _disconnect_rx) = mpsc::channel(1);
    let (recipient_disconnect, _recipient_disconnect_rx) = mpsc::channel(1);
    for (peer, tx, private, session_disconnect) in [
        (&a, &atx, &ap, &disconnect),
        (&b, &btx, &bp, &recipient_disconnect),
    ] {
        wr.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            role: peer.role,
            tx: tx.clone(),
            disconnect: session_disconnect.clone(),
        });
        wr.action_targets.insert(
            peer.device_public_key,
            ActionTarget {
                broker_host: None,
                broker_protocol: false,
                room,
                peer: peer.clone(),
                state_tx: tx.clone(),
                tx: private.clone(),
            },
        );
    }
    let wr = Arc::new(AsyncMutex::new(wr));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, wr.clone())])));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &atx,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &a.cert,
        own_device_public_key: &a.cert.device_public_key,
        app_serving: None,
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &disconnect,
        presence: None,
        private_tx: &ap,
        revocations: &revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    let receiver = SessionCtx {
        outbound: &btx,
        private_tx: &bp,
        disconnect: &recipient_disconnect,
        ..session
    };
    let action = DirectedAction {
        room,
        request_id: uuid::Uuid::from_u128(8),
        source: a.device_public_key,
        target: b.device_public_key,
        message: ActionMessage::CatalogueRefresh { scope: None },
    };

    let root_path = wr.lock().await.store.root().to_path_buf();
    let (release, reached) = crate::store::test_barrier::install(&root_path, "membership-read");
    let observed = wr.clone();
    let device = a.device_public_key;
    let replacement_disconnect = disconnect.clone();
    let handle = tokio::runtime::Handle::current();
    let observer = std::thread::spawn(move || {
        handle.block_on(async move {
            reached.await.unwrap();
            let lock = tokio::time::timeout(tilo_test_env::hang_bound(), observed.lock()).await;
            let unlocked = if let Ok(mut runtime) = lock {
                let (replacement, _rx) = mpsc::channel(2);
                runtime.add_subscriber(Subscriber {
                    device_public_key: device,
                    role: crate::auth::PeerRole::Device,
                    tx: replacement,
                    disconnect: replacement_disconnect,
                });
                true
            } else {
                false
            };
            drop(release);
            unlocked
        })
    });
    let result = route_private(&action, &a, &session).await.unwrap();
    assert!(
        observer.join().unwrap(),
        "fresh membership read held routing lock"
    );
    assert_eq!(result, Some(UnavailableReason::NotAuthorised));
    assert!(bpr.try_recv().is_err());
    let _ = receiver;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authorization_rechecks_certificates_after_blocking_membership_read() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (a, _a_key) = certified_device(&root, user.public_key, "a");
    let (b, _) = certified_device(&root, user.public_key, "b");
    let room = uuid::Uuid::from_u128(7);
    let id = crate::room_workspace_id(&room.to_string()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut wr = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    wr.admit_device(a.device_public_key).unwrap();
    wr.admit_device(b.device_public_key).unwrap();
    let (atx, _arx) = mpsc::channel(2);
    let (btx, _brx) = mpsc::channel(2);
    let (ap, _apr) = mpsc::channel(2);
    let (bp, mut bpr) = mpsc::channel(1);
    let (disconnect, _disconnect_rx) = mpsc::channel(1);
    let (recipient_disconnect, _recipient_disconnect_rx) = mpsc::channel(1);
    for (peer, tx, private, session_disconnect) in [
        (&a, &atx, &ap, &disconnect),
        (&b, &btx, &bp, &recipient_disconnect),
    ] {
        wr.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            role: peer.role,
            tx: tx.clone(),
            disconnect: session_disconnect.clone(),
        });
        wr.action_targets.insert(
            peer.device_public_key,
            ActionTarget {
                broker_host: None,
                broker_protocol: false,
                room,
                peer: peer.clone(),
                state_tx: tx.clone(),
                tx: private.clone(),
            },
        );
    }
    let wr = Arc::new(AsyncMutex::new(wr));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, wr.clone())])));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &atx,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &a.cert,
        own_device_public_key: &a.cert.device_public_key,
        app_serving: None,
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &disconnect,
        presence: None,
        private_tx: &ap,
        revocations: &revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    let receiver = SessionCtx {
        outbound: &btx,
        private_tx: &bp,
        disconnect: &recipient_disconnect,
        ..session
    };
    let action = DirectedAction {
        room,
        request_id: uuid::Uuid::from_u128(8),
        source: a.device_public_key,
        target: b.device_public_key,
        message: ActionMessage::CatalogueRefresh { scope: None },
    };

    assert_eq!(route_private(&action, &a, &session).await.unwrap(), None);
    let PrivateOutbound::Delivery(delivery) = bpr.recv().await.unwrap() else {
        panic!("delivery");
    };
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: user.public_key,
        revoked_device_public_key: a.device_public_key,
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Lost,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    let root_path = wr.lock().await.store.root().to_path_buf();
    for operation in 0..5 {
        *revocations.write().unwrap() = RevocationList::new();
        let (release, reached) = crate::store::test_barrier::install(&root_path, "membership-read");
        let list = revocations.clone();
        let entry = entry.clone();
        let handle = tokio::runtime::Handle::current();
        let revoke = std::thread::spawn(move || {
            handle.block_on(async move {
                reached.await.unwrap();
                list.write().unwrap().insert(entry).unwrap();
                drop(release);
            });
        });
        let denied = match operation {
            0 => {
                route_private(&action, &a, &session).await.unwrap()
                    == Some(UnavailableReason::NotAuthorised)
            }
            1 => {
                authorize_delivery(&delivery, &b, &receiver).await
                    == Err(UnavailableReason::NotAuthorised)
            }
            2 => handle_private(EnvelopeV2::EnableActions { room }, &a, &session)
                .await
                .is_err(),
            3 => handle_private(EnvelopeV2::EnableBrokerClient { room }, &a, &session)
                .await
                .is_err(),
            _ => handle_private(
                EnvelopeV2::RegisterBroker {
                    room,
                    host: uuid::Uuid::from_u128(99),
                },
                &a,
                &session,
            )
            .await
            .is_err(),
        };
        revoke.join().unwrap();
        assert!(
            denied,
            "operation {operation} authorized a certificate revoked during its membership read"
        );
    }
}

/// Counts what reaches the app-serving plane; needs no reply.
#[derive(Default)]
struct CountingPlane(std::sync::atomic::AtomicUsize);

impl crate::app_serving::AppServingPlane for CountingPlane {
    fn serve(
        &self,
        _workspace_id: u64,
        _caller: &tilo_actions::remote::Caller,
        _message: tilo_actions::remote::RemoteActionMessage,
    ) -> Option<tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage>> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        None
    }
}

#[tokio::test]
async fn replacement_action_capability_requires_enable_then_durable_registration() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (source, _) = certified_device(&root, user.public_key, "source");
    let (broker, _) = certified_device(&root, user.public_key, "broker");
    let room = uuid::Uuid::from_u128(7);
    let host = uuid::Uuid::from_u128(99);
    let id = crate::room_workspace_id(&room.to_string()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    runtime.admit_device(source.device_public_key).unwrap();
    runtime.admit_device(broker.device_public_key).unwrap();
    let (source_state, _source_state_rx) = mpsc::channel(2);
    let (old_broker_state, _old_broker_state_rx) = mpsc::channel(2);
    let (replacement_state, _replacement_state_rx) = mpsc::channel(2);
    let (source_private, _source_private_rx) = mpsc::channel(2);
    let (broker_private, mut broker_private_rx) = mpsc::channel(2);
    let (source_disconnect, _source_disconnect_rx) = mpsc::channel(1);
    let (broker_disconnect, _broker_disconnect_rx) = mpsc::channel(1);
    runtime.add_subscriber(Subscriber {
        device_public_key: source.device_public_key,
        role: source.role,
        tx: source_state.clone(),
        disconnect: source_disconnect.clone(),
    });
    runtime.add_subscriber(Subscriber {
        device_public_key: broker.device_public_key,
        role: broker.role,
        tx: old_broker_state.clone(),
        disconnect: broker_disconnect.clone(),
    });
    let runtime = Arc::new(AsyncMutex::new(runtime));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, runtime.clone())])));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let source_session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &source_state,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &source.cert,
        own_device_public_key: &source.cert.device_public_key,
        app_serving: None,
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &source_disconnect,
        presence: None,
        private_tx: &source_private,
        revocations: &revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    assert!(matches!(
        handle_private(EnvelopeV2::EnableActions { room }, &source, &source_session)
            .await
            .unwrap(),
        Some(EnvelopeV2::ActionsReady {
            capability: ActionCapability::TransportOnly,
            ..
        })
    ));
    assert!(
        handle_private(
            EnvelopeV2::EnableBrokerClient { room },
            &source,
            &source_session
        )
        .await
        .unwrap()
        .is_none()
    );

    runtime.lock().await.add_subscriber(Subscriber {
        device_public_key: broker.device_public_key,
        role: broker.role,
        tx: replacement_state.clone(),
        disconnect: broker_disconnect.clone(),
    });
    let replacement_session = SessionCtx {
        outbound: &replacement_state,
        private_tx: &broker_private,
        disconnect: &broker_disconnect,
        ..source_session
    };
    let action = DirectedAction {
        room,
        request_id: uuid::Uuid::from_u128(8),
        source: source.device_public_key,
        target: broker.device_public_key,
        message: ActionMessage::InvokeBound {
            host,
            invocation: tilo_actions::InvocationId::new(),
            app_peer: "1".into(),
            app_id: "so.tilo.test".into(),
            record_id: "record".into(),
            resource: "record".into(),
            action: "test".into(),
            argument: None,
            confirmation: None,
            revision: None,
        },
    };
    assert_eq!(
        route_private(&action, &source, &source_session)
            .await
            .unwrap(),
        Some(UnavailableReason::UnsupportedPeer)
    );
    assert!(broker_private_rx.try_recv().is_err());

    assert_eq!(
        handle_private(
            EnvelopeV2::EnableActions { room },
            &broker,
            &replacement_session
        )
        .await
        .unwrap(),
        Some(EnvelopeV2::ActionsReady {
            room,
            capability: ActionCapability::TransportOnly,
        })
    );
    assert_eq!(
        route_private(&action, &source, &source_session)
            .await
            .unwrap(),
        Some(UnavailableReason::AdmissionUnavailable)
    );
    assert!(broker_private_rx.try_recv().is_err());

    assert_eq!(
        handle_private(
            EnvelopeV2::RegisterBroker { room, host },
            &broker,
            &replacement_session
        )
        .await
        .unwrap(),
        Some(EnvelopeV2::ActionsReady {
            room,
            capability: ActionCapability::DurableBroker { host },
        })
    );
    assert_eq!(
        route_private(&action, &source, &source_session)
            .await
            .unwrap(),
        None
    );
    assert!(matches!(
        broker_private_rx.recv().await,
        Some(PrivateOutbound::Delivery(_))
    ));

    runtime
        .lock()
        .await
        .remove_subscriber_session(&broker.device_public_key, &old_broker_state);
    let current = runtime.lock().await;
    assert!(
        current
            .subscribers
            .get(&broker.device_public_key)
            .is_some_and(|subscriber| subscriber.tx.same_channel(&replacement_state))
    );
    assert!(
        current
            .action_targets
            .get(&broker.device_public_key)
            .is_some_and(|target| target.broker_host == Some(host)
                && target.state_tx.same_channel(&replacement_state))
    );
    drop(current);
    assert_eq!(
        route_private(&action, &source, &source_session)
            .await
            .unwrap(),
        None
    );
    assert!(matches!(
        broker_private_rx.recv().await,
        Some(PrivateOutbound::Delivery(_))
    ));
}

#[tokio::test]
async fn bounded_private_queue_and_delivery_recheck_never_retain_traffic() {
    Box::pin(bounded_private_queue_for_room_namespace()).await;
}

async fn bounded_private_queue_for_room_namespace() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (a, a_key) = certified_device(&root, user.public_key, "a");
    let (b, _) = certified_device(&root, user.public_key, "b");
    let room = uuid::Uuid::from_u128(7);
    let id = crate::room_workspace_id(&room.to_string()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut wr = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    wr.admit_device(a.device_public_key).unwrap();
    wr.admit_device(b.device_public_key).unwrap();
    let (atx, _arx) = mpsc::channel(2);
    let (btx, _brx) = mpsc::channel(2);
    let (ap, mut apr) = mpsc::channel(2);
    let (bp, mut bpr) = mpsc::channel(1);
    let (disconnect, mut disconnect_rx) = mpsc::channel(1);
    let (recipient_disconnect, mut recipient_disconnect_rx) = mpsc::channel(1);
    for (peer, tx, private, session_disconnect) in [
        (&a, &atx, &ap, &disconnect),
        (&b, &btx, &bp, &recipient_disconnect),
    ] {
        wr.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            role: peer.role,
            tx: tx.clone(),
            disconnect: session_disconnect.clone(),
        });
        wr.action_targets.insert(
            peer.device_public_key,
            ActionTarget {
                broker_host: None,
                broker_protocol: false,
                room,
                peer: peer.clone(),
                state_tx: tx.clone(),
                tx: private.clone(),
            },
        );
    }
    let wr = Arc::new(AsyncMutex::new(wr));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, wr.clone())])));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &atx,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &a.cert,
        own_device_public_key: &a.cert.device_public_key,
        app_serving: None,
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &disconnect,
        presence: None,
        private_tx: &ap,
        revocations: &revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    let receiver = SessionCtx {
        outbound: &btx,
        private_tx: &bp,
        disconnect: &recipient_disconnect,
        ..session
    };
    let action = DirectedAction {
        room,
        request_id: uuid::Uuid::from_u128(8),
        source: a.device_public_key,
        target: b.device_public_key,
        message: ActionMessage::CatalogueRefresh { scope: None },
    };
    assert!(
        !private_may_overtake(
            &EnvelopeV2::Directed(action.clone()),
            Some(id),
            std::iter::empty(),
        ),
        "private traffic must not overtake its room's state control"
    );
    // An old-v2 caller never receives an unknown broker message, even
    // when it targets an endpoint that installed the extended facade.
    wr.lock()
        .await
        .action_targets
        .get_mut(&b.device_public_key)
        .unwrap()
        .broker_host = Some(uuid::Uuid::from_u128(99));
    assert_eq!(
        route_private(&action, &a, &session).await.unwrap(),
        Some(UnavailableReason::UnsupportedPeer)
    );
    assert!(bpr.try_recv().is_err());
    wr.lock()
        .await
        .action_targets
        .get_mut(&b.device_public_key)
        .unwrap()
        .broker_host = None;
    // State resubscribe is idempotent on the same connection. It must not
    // silently withdraw a separately negotiated private capability.
    wr.lock().await.add_subscriber(Subscriber {
        device_public_key: b.device_public_key,
        role: b.role,
        tx: btx.clone(),
        disconnect: recipient_disconnect.clone(),
    });
    assert_eq!(route_private(&action, &a, &session).await.unwrap(), None);
    assert_eq!(
        route_private(&action, &a, &session).await.unwrap(),
        Some(UnavailableReason::Backpressure)
    );
    let PrivateOutbound::Delivery(delivery) = bpr.recv().await.unwrap() else {
        panic!("expected private delivery")
    };
    assert_eq!(authorize_delivery(&delivery, &b, &receiver).await, Ok(()));
    // Sender removal also invalidates an already queued private result.
    wr.lock()
        .await
        .store
        .save_membership(&[b.device_public_key])
        .unwrap();
    assert_eq!(
        route_private(&action, &a, &session).await.unwrap(),
        Some(UnavailableReason::NotAuthorised)
    );
    let mut sender_removed_wire = Vec::new();
    write_private_delivery(&delivery, &b, &receiver, &mut sender_removed_wire)
        .await
        .unwrap();
    assert!(sender_removed_wire.is_empty());
    assert!(matches!(
        apr.recv().await,
        Some(PrivateOutbound::Reply(EnvelopeV2::Unavailable {
            reason: UnavailableReason::NotAuthorised,
            ..
        }))
    ));
    wr.lock()
        .await
        .store
        .save_membership(&[a.device_public_key, b.device_public_key])
        .unwrap();

    // Root-signed revocation between dispatch and actual writer delivery.
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: user.public_key,
        revoked_device_public_key: a.device_public_key,
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Lost,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    revocations.write().unwrap().insert(entry).unwrap();
    let mut revoked_wire = Vec::new();
    write_private_delivery(&delivery, &b, &receiver, &mut revoked_wire)
        .await
        .unwrap();
    assert!(revoked_wire.is_empty());
    assert!(matches!(
        apr.recv().await,
        Some(PrivateOutbound::Reply(EnvelopeV2::Unavailable {
            reason: UnavailableReason::NotAuthorised,
            ..
        }))
    ));
    *revocations.write().unwrap() = RevocationList::new();

    // Expiry is rechecked at the writer, after the authenticated source's
    // queued delivery was created. Re-sign the short-lived certificate so
    // the fixture still uses a valid root-issued chain.
    let mut expiring = delivery.clone();
    expiring.source.cert.expires_at_ns = Some(unix_now_ns().saturating_add(100_000_000));
    sign_device_cert(&mut expiring.source.cert, &root).unwrap();
    expiring.source = crate::auth::accept_hello(
        &crate::auth::Hello::new(expiring.source.cert.clone(), [9; 32]),
        &user.public_key,
        &crate::policy::TrustPolicy::AllowMatchingUser,
        &RevocationList::new(),
        unix_now_ns(),
    )
    .unwrap();
    crate::auth::verify_hello_ack2(
        &crate::auth::build_hello_ack2(&a_key, &[8; 32]),
        &expiring.source.device_public_key,
        &[8; 32],
    )
    .unwrap();
    assert_eq!(authorize_delivery(&expiring, &b, &receiver).await, Ok(()));
    tokio::time::sleep(std::time::Duration::from_millis(110)).await;
    let mut expired_wire = Vec::new();
    write_private_delivery(&expiring, &b, &receiver, &mut expired_wire)
        .await
        .unwrap();
    assert!(expired_wire.is_empty());
    assert!(matches!(
        apr.recv().await,
        Some(PrivateOutbound::Reply(EnvelopeV2::Unavailable {
            reason: UnavailableReason::NotAuthorised,
            ..
        }))
    ));

    // Host certificates retain their PeerRelay role; no facade exception.
    let mut relay_peer = a.clone();
    relay_peer.cert.capabilities = DeviceCapabilities::host();
    sign_device_cert(&mut relay_peer.cert, &root).unwrap();
    relay_peer = crate::auth::accept_hello(
        &crate::auth::Hello::new(relay_peer.cert.clone(), [7; 32]),
        &user.public_key,
        &crate::policy::TrustPolicy::AllowMatchingUser,
        &RevocationList::new(),
        unix_now_ns(),
    )
    .unwrap();
    crate::auth::verify_hello_ack2(
        &crate::auth::build_hello_ack2(&a_key, &[6; 32]),
        &relay_peer.device_public_key,
        &[6; 32],
    )
    .unwrap();
    assert_eq!(relay_peer.role, crate::auth::PeerRole::PeerRelay);
    assert_eq!(
        route_private(&action, &relay_peer, &session).await.unwrap(),
        Some(UnavailableReason::NotAuthorised)
    );

    // Removal after dispatch but before recipient wire delivery fails closed.
    wr.lock()
        .await
        .store
        .save_membership(&[a.device_public_key])
        .unwrap();
    assert_eq!(
        authorize_delivery(&delivery, &b, &receiver).await,
        Err(UnavailableReason::NotAuthorised)
    );
    wr.lock()
        .await
        .store
        .save_membership(&[a.device_public_key, b.device_public_key])
        .unwrap();
    // Exact room equality is checked in addition to the numeric shard.
    wr.lock()
        .await
        .action_targets
        .get_mut(&b.device_public_key)
        .unwrap()
        .room = uuid::Uuid::from_u128(9);
    assert_eq!(
        route_private(&action, &a, &session).await.unwrap(),
        Some(UnavailableReason::NotAuthorised)
    );
    wr.lock()
        .await
        .action_targets
        .get_mut(&b.device_public_key)
        .unwrap()
        .room = room;
    let (replacement, _rx) = mpsc::channel(2);
    wr.lock().await.add_subscriber(Subscriber {
        device_public_key: b.device_public_key,
        role: b.role,
        tx: replacement.clone(),
        disconnect: recipient_disconnect.clone(),
    });
    assert_eq!(
        authorize_delivery(&delivery, &b, &receiver).await,
        Err(UnavailableReason::NotAuthorised)
    );
    wr.lock()
        .await
        .remove_subscriber_session(&b.device_public_key, &btx);
    assert!(
        wr.lock()
            .await
            .subscribers
            .get(&b.device_public_key)
            .unwrap()
            .tx
            .same_channel(&replacement)
    );
    // A recipient-side rejection must reach the original source, or close
    // that exact session when its reply lane has no remaining capacity.
    let mut wire = Vec::new();
    write_private_delivery(&delivery, &b, &receiver, &mut wire)
        .await
        .unwrap();
    assert!(wire.is_empty());
    let Some(PrivateOutbound::Reply(reply)) = apr.recv().await else {
        panic!("missing rejection")
    };
    assert_eq!(
        reply,
        EnvelopeV2::unavailable(&action, UnavailableReason::NotAuthorised)
    );
    for _ in 0..2 {
        ap.try_send(PrivateOutbound::Reply(reply.clone()))
            .ok()
            .unwrap();
    }
    let (new_source, new_source_rx) = mpsc::channel(2);
    let (new_disconnect, mut new_disconnect_rx) = mpsc::channel(1);
    wr.lock().await.add_subscriber(Subscriber {
        device_public_key: a.device_public_key,
        role: a.role,
        tx: new_source.clone(),
        disconnect: new_disconnect,
    });
    write_private_delivery(&delivery, &b, &receiver, &mut wire)
        .await
        .unwrap();
    assert!(
        wire.is_empty(),
        "rejected private body must not reach recipient wire"
    );
    assert_eq!(
        disconnect_rx.try_recv(),
        Ok(()),
        "a saturated rejection queue must terminate its original source session"
    );
    assert!(
        matches!(
            new_disconnect_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ),
        "replacement source session must not be terminated"
    );
    assert!(!new_source_rx.is_closed());
    assert!(
        matches!(
            recipient_disconnect_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ),
        "recipient session must not be terminated for source backpressure"
    );
    let wr = wr.lock().await;
    assert_eq!(wr.store.count_pending(&a.device_public_key).unwrap(), 0);
    assert_eq!(wr.store.count_pending(&b.device_public_key).unwrap(), 0);
    assert!(wr.store.load_retained().unwrap().is_empty());
    assert!(wr.store.load_snapshot().unwrap().is_none());
}

#[tokio::test]
async fn a_device_presence_frame_rearms_its_live_checkpoint() {
    // Wiring for the per-device live checkpoint gate: after a device has been served one live
    // checkpoint the relay stops fanning checkpoints to it; a presence frame
    // from that device (the browser's hello / SyncReq / rejected-checkpoint
    // path) must re-arm exactly that device so the native's fresh
    // checkpoint reaches it.
    let (root, user) = UserIdentity::generate("alice", 1);
    let (native, _) = certified_device(&root, user.public_key, "native");
    let (browser, _) = certified_device(&root, user.public_key, "browser");
    let room = uuid::Uuid::from_u128(7);
    let id = crate::room_workspace_id(&room.to_string()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut wr = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        false,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    wr.admit_device(native.device_public_key).unwrap();
    wr.admit_device(browser.device_public_key).unwrap();
    let (btx, mut brx) = mpsc::channel(4);
    let (bdis, _bdis_rx) = mpsc::channel(1);
    wr.add_subscriber(Subscriber {
        device_public_key: browser.device_public_key,
        role: browser.role,
        tx: btx.clone(),
        disconnect: bdis.clone(),
    });
    // Serve the subscribe-time need with one checkpoint from the native.
    let raw = crate::coedit::CoeditPayload::Checkpoint {
        channel: [1; 32],
        body: vec![1],
    }
    .encode()
    .unwrap();
    let checkpoint_at = |authored_at_ns: u64| Envelope::Delta {
        workspace_id: id,
        author_device_public_key: native.device_public_key,
        authored_at_ns,
        delta: raw.clone(),
        signature: [0; 64],
    };
    let env = checkpoint_at(1);
    wr.fanout_delta(&native.device_public_key, native.role, &env, &raw, 1)
        .unwrap();
    brx.try_recv().unwrap();
    assert!(!wr.needs_checkpoint(&browser.device_public_key, &[1; 32]));

    let wr = Arc::new(AsyncMutex::new(wr));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, wr.clone())])));
    let trust = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowMatchingUser,
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let (ptx, _prx) = mpsc::channel(1);
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &btx,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &browser.cert,
        own_device_public_key: &browser.cert.device_public_key,
        app_serving: None,
        metrics: &metrics,
        trust_policy: &trust,
        disconnect: &bdis,
        presence: None,
        private_tx: &ptx,
        revocations: &revocations,
        live_bridge: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    // The browser sends a SyncReq as presence.
    handle_presence(
        &browser,
        &session,
        &[id],
        Envelope::Presence {
            workspace_id: id,
            payload: vec![9, 9],
        },
    )
    .await
    .unwrap();
    assert!(
        wr.lock()
            .await
            .needs_checkpoint(&browser.device_public_key, &[1; 32]),
        "a device presence frame must re-arm its live checkpoint"
    );
    // The native's fresh checkpoint now reaches it, once.
    let env2 = checkpoint_at(2);
    wr.lock()
        .await
        .fanout_delta(&native.device_public_key, native.role, &env2, &raw, 2)
        .unwrap();
    assert!(brx.try_recv().is_ok());
    let env3 = checkpoint_at(3);
    wr.lock()
        .await
        .fanout_delta(&native.device_public_key, native.role, &env3, &raw, 3)
        .unwrap();
    assert!(brx.try_recv().is_err(), "satisfied by one checkpoint");

    let (native_tx, mut native_rx) = mpsc::channel(4);
    let (native_disconnect, _native_disconnect_rx) = mpsc::channel(1);
    wr.lock().await.add_subscriber(Subscriber {
        device_public_key: native.device_public_key,
        role: native.role,
        tx: native_tx,
        disconnect: native_disconnect,
    });
    // The browser claims another admitted device's author identity.
    let forged = crate::AttributedPresence {
        author: native.device_public_key,
        body: b"fixture presence".to_vec(),
    };
    handle_presence(
        &browser,
        &session,
        &[id],
        Envelope::Presence {
            workspace_id: id,
            payload: forged.encode().unwrap(),
        },
    )
    .await
    .unwrap();
    let Envelope::Presence { payload, .. } = native_rx.try_recv().unwrap() else {
        panic!("expected forwarded presence");
    };
    let observed = crate::AttributedPresence::decode(&payload).unwrap();
    assert_eq!(observed.author, browser.device_public_key);
    assert_eq!(observed.body, forged.body);
}
