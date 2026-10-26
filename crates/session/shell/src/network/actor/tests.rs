use super::*;

#[tokio::test]
async fn encoded_backlog_stale_completion_cannot_finish_a_restarted_stream() {
    let key = (42, 31);
    let mut tasks = SurfaceFanoutTasks::new();
    tasks.insert(key, (1, tokio::spawn(std::future::pending())));
    let (_, old) = tasks.remove(&key).expect("old stream task");
    old.abort();
    let current = tokio::spawn(std::future::pending());
    let current_abort = current.abort_handle();
    tasks.insert(key, (2, current));
    assert!(!finish_surface_fanout(&mut tasks, key, 1));
    assert!(
        tasks.contains_key(&key),
        "old completion retains the new task"
    );
    assert!(finish_surface_fanout(&mut tasks, key, 2));
    current_abort.abort();
    assert!(tasks.is_empty());
    assert!(!finish_surface_fanout(&mut tasks, key, 2));
}

#[test]
fn encoded_backlog_accounts_for_retained_allocation_after_pop_and_refill() {
    let mut backlog = SurfaceFrameBacklog::default();
    for cycle in 0..4 {
        for ts in 0..100 {
            backlog.push(NeighbourMsg::SurfaceFrame {
                surface: 31,
                source_peer: 1,
                ts,
                encoded_h264: vec![0x41; 1024],
                seq: ts,
            });
            let allocated = backlog
                .frames
                .capacity()
                .saturating_mul(std::mem::size_of::<NeighbourMsg>())
                .saturating_add(
                    backlog
                        .frames
                        .iter()
                        .map(|msg| match msg {
                            NeighbourMsg::SurfaceFrame { encoded_h264, .. } => {
                                encoded_h264.capacity()
                            }
                            _ => 0,
                        })
                        .sum::<usize>(),
                );
            assert!(
                allocated <= backlog.bytes && allocated <= MAX_SURFACE_FRAME_BYTES,
                "cycle={cycle} accounted={} allocated={allocated}",
                backlog.bytes
            );
        }
        for _ in 0..75 {
            assert!(backlog.pop().is_some());
            let allocated = backlog
                .frames
                .capacity()
                .saturating_mul(std::mem::size_of::<NeighbourMsg>())
                .saturating_add(
                    backlog
                        .frames
                        .iter()
                        .map(|msg| match msg {
                            NeighbourMsg::SurfaceFrame { encoded_h264, .. } => {
                                encoded_h264.capacity()
                            }
                            _ => 0,
                        })
                        .sum::<usize>(),
                );
            assert!(allocated <= backlog.bytes && allocated <= MAX_SURFACE_FRAME_BYTES);
        }
    }
}

#[test]
fn encoded_backlog_preserves_order_and_recovers_at_a_keyframe_after_loss() {
    let frame = |ts, encoded_h264| NeighbourMsg::SurfaceFrame {
        surface: 31,
        source_peer: 1,
        encoded_h264,
        ts,
        seq: ts,
    };
    let mut backlog = SurfaceFrameBacklog::default();
    for ts in 1..4 {
        backlog.push(frame(ts, vec![0, 0, 1, 0x41]));
    }
    for expected in 1..4 {
        assert!(
            matches!(backlog.pop(), Some(NeighbourMsg::SurfaceFrame { ts, .. }) if ts == expected)
        );
    }
    assert_eq!(backlog.bytes, 0);
    // Fill the same production byte budget, including queue storage.
    let payload = vec![
        0x41;
        MAX_SURFACE_FRAME_BYTES
            .saturating_sub(std::mem::size_of::<NeighbourMsg>().saturating_mul(4))
    ];
    backlog.push(frame(4, payload));
    assert_eq!(backlog.bytes, MAX_SURFACE_FRAME_BYTES);
    backlog.push(frame(5, vec![0, 0, 1, 0x41]));
    assert!(backlog.frames.is_empty());
    assert_eq!(backlog.bytes, 0);
    assert!(backlog.awaiting_keyframe);
    backlog.push(frame(6, vec![0, 0, 1, 0x41]));
    assert!(backlog.pop().is_none(), "lost references cannot be used");
    backlog.push(frame(7, vec![0, 0, 1, 0x65]));
    assert!(matches!(
        backlog.pop(),
        Some(NeighbourMsg::SurfaceFrame { ts: 7, .. })
    ));
    assert!(!backlog.awaiting_keyframe);
    // A failed send uses this same production recovery transition.
    backlog.lose_references();
    backlog.codec = Some(media::StreamCodec::Vp9);
    assert!(
        !backlog.admit(&frame(8, vec![0, 0, 1, 0x65])),
        "H264 is not a VP9 keyframe"
    );
    assert!(backlog.admit(&frame(9, vec![0x82, 0x49, 0x83, 0x42])));
    backlog.codec = None;
    backlog.lose_references();
    assert!(
        !backlog.awaiting_keyframe,
        "independent pictures need no keyframe"
    );
    backlog.push(frame(10, vec![0xff, 0xd8]));
    backlog.push(frame(11, vec![0xff, 0xd8]));
    assert!(matches!(
        backlog.pop(),
        Some(NeighbourMsg::SurfaceFrame { ts: 11, .. })
    ));
    assert!(
        backlog.pop().is_none(),
        "independent pictures keep newest only"
    );
}

/// A dial wakes the relays; what a lone desktop does on every local
/// change (join its own topic, publish to it) does not.
#[test]
fn only_a_remote_party_needs_this_device_reachable() {
    let addr = EndpointAddr::new(iroh::SecretKey::generate().public());
    assert!(remote_party_command(&NetCmd::Connect { addr: addr.clone() }).is_some());
    assert!(
        remote_party_command(&NetCmd::ConnectHost {
            addr,
            expectation: None,
        })
        .is_some()
    );
    let (on_event, _events) = mpsc::channel(1);
    assert!(remote_party_command(&NetCmd::JoinWorkspace { ws: 7, on_event }).is_none());
    assert!(remote_party_command(&NetCmd::LeaveWorkspace { ws: 7 }).is_none());
}

#[test]
fn a_remote_action_with_no_route_waits_for_that_peer_and_expires() {
    let mut pending = PendingRemoteActions::default();
    let peer = iroh::SecretKey::generate().public();
    let other = iroh::SecretKey::generate().public();
    let start = Instant::now();
    pending.hold(1, peer, b"open".to_vec(), start);
    pending.hold(1, other, b"other".to_vec(), start);
    pending.hold(1, peer, b"again".to_vec(), start);
    assert_eq!(pending.targets(), vec![(1, peer), (1, other)]);
    assert_eq!(
        pending.take_for(1, peer, start),
        vec![b"open".to_vec(), b"again".to_vec()]
    );
    assert!(
        pending.take_for(2, other, start).is_empty(),
        "held per workspace"
    );
    assert_eq!(pending.take_for(1, other, start), vec![b"other".to_vec()]);

    for index in 0..MAX_PENDING_REMOTE_ACTIONS {
        pending.hold(2, peer, vec![u8::try_from(index).unwrap()], start);
    }
    pending.hold(2, peer, b"newest".to_vec(), start);
    let kept = pending.take_for(2, peer, start);
    assert_eq!(kept.len(), MAX_PENDING_REMOTE_ACTIONS);
    assert_eq!(kept[0], vec![1]);
    assert_eq!(kept.last().expect("kept"), b"newest");

    pending.hold(3, peer, b"stale".to_vec(), start);
    assert!(
        pending
            .take_for(3, peer, start + PENDING_REMOTE_ACTION_TTL)
            .is_empty(),
        "a request is not run long after it was sent"
    );
}

#[test]
fn upkeep_is_armed_only_while_something_needs_it() {
    const EVERY: Duration = Duration::from_secs(5);
    let before = Instant::now();
    assert_eq!(
        next_upkeep(None, false, EVERY),
        None,
        "alone, it never ticks"
    );
    let armed = next_upkeep(None, true, EVERY).expect("a peer arms it");
    assert!(
        armed >= later(before, EVERY),
        "the first tick is a whole interval away"
    );
    assert_eq!(
        next_upkeep(Some(armed), true, EVERY),
        Some(armed),
        "an armed tick keeps its deadline"
    );
    assert_eq!(
        next_upkeep(Some(armed), false, EVERY),
        None,
        "the last peer gone disarms it"
    );
}

/// What a desktop alone with its rooms broadcasts reaches nobody, so it
/// never arms gossip's per-topic cache sweep, which would otherwise run
/// until the message expires (`gossip_caches_drain_within`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_topic_with_no_neighbour_never_arms_the_gossip_sweep() {
    const SWEEP: Duration = Duration::from_millis(20);
    let endpoint = Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let gossip = Gossip::builder()
        .broadcast_config(iroh_gossip::proto::PlumtreeConfig {
            cache_evict_interval: SWEEP,
            ..broadcast_config()
        })
        .spawn(endpoint.clone());
    let (sender, _receiver) = gossip
        .subscribe(topic_id(42), Vec::new())
        .await
        .unwrap()
        .split();
    let timers = || gossip.metrics().actor_tick_timers.get();

    for _ in 0..3 {
        sender
            .broadcast(Bytes::from_static(b"a local change"))
            .await
            .unwrap();
        sender
            .broadcast_neighbors(Bytes::from_static(b"an identity"))
            .await
            .unwrap();
    }
    tokio::time::sleep(SWEEP.saturating_mul(10)).await;
    assert_eq!(timers(), 0, "gossip woke for a topic nobody else is on");
    endpoint.close().await;
}
use getrandom::{SysRng, rand_core::UnwrapErr};

#[test]
fn a_repeated_click_is_new_but_its_copy_over_another_route_is_not() {
    let mut recent = HashMap::new();
    let host = iroh::SecretKey::from_bytes(&[0xA1; 32]).public();
    let other = iroh::SecretKey::from_bytes(&[0xA2; 32]).public();
    let start = Instant::now();
    assert!(mark_new_input(1, host, [7; 32], start, &mut recent));
    assert!(
        !mark_new_input(
            1,
            other,
            [7; 32],
            start + Duration::from_millis(1),
            &mut recent
        ),
        "the same event arriving over a second relay session"
    );
    assert!(
        mark_new_input(
            1,
            host,
            [7; 32],
            start + Duration::from_millis(200),
            &mut recent
        ),
        "a second, byte-identical click"
    );
    assert!(mark_new_input(
        2,
        host,
        [7; 32],
        start + Duration::from_millis(200),
        &mut recent
    ));
    // "ll" typed in one batch: both strokes come over one session at once.
    for _ in 0..2 {
        assert!(
            mark_new_input(
                1,
                host,
                [9; 32],
                start + Duration::from_millis(300),
                &mut recent
            ),
            "a repeated key over the same session"
        );
    }
}

fn active_personal_transfer(generation: u64) -> ActivePersonalTransfer {
    let (cancel, _receiver) = tokio::sync::watch::channel(false);
    ActivePersonalTransfer {
        generation,
        workspace: 7,
        total_bytes: 8,
        sent_bytes: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        cancel,
    }
}

#[test]
fn personal_bindings_are_exact_generation_and_membership_scoped() {
    // Phase `personal_transfer_generation_is_exact_and_stale_completion_cannot_remove_replacement`.
    {
        let first = iroh::SecretKey::from_bytes(&[0xB1; 32]).public();
        let sibling = iroh::SecretKey::from_bytes(&[0xB2; 32]).public();
        let transfer_id = [0xB3; 16];
        let key = (first, transfer_id);
        let sibling_key = (sibling, transfer_id);
        let mut active = HashMap::from([
            (key, active_personal_transfer(12)),
            (sibling_key, active_personal_transfer(4)),
        ]);

        assert!(take_personal_transfer_generation(&mut active, key, 11).is_none());
        assert!(
            active.contains_key(&key),
            "old worker cannot remove replacement"
        );
        assert!(
            take_personal_transfer_generation(&mut active, sibling_key, 12).is_none(),
            "same id on another target is isolated"
        );
        assert!(take_personal_transfer_generation(&mut active, key, 12).is_some());
        assert!(active.contains_key(&sibling_key));
    }
    // Phase `departed_personal_connection_cannot_rebind_after_membership_reconnect`.
    {
        let peer = iroh::SecretKey::from_bytes(&[0xA1; 32]).public();
        let old_epoch = MembershipEpoch::for_test(7);
        let new_epoch = MembershipEpoch::for_test(8);
        let mut ingress = HashMap::new();
        assert!(bind_personal_fabric_ingress(
            &mut ingress,
            peer,
            11,
            21,
            old_epoch,
        ));
        revoke_personal_fabric_ingress(&mut ingress, 21, old_epoch);
        assert!(
            !bind_personal_fabric_ingress(&mut ingress, peer, 11, 21, new_epoch),
            "the old QUIC connection stays tombstoned after reconnect"
        );
        assert!(
            bind_personal_fabric_ingress(&mut ingress, peer, 12, 21, new_epoch),
            "a fresh connection may bind the fresh membership epoch"
        );
    }
    // Phase `nearby_transfer_frame_round_trips_exact_binding_and_bounded_chunk`.
    {
        let wire = PersonalFabricWireFrame {
            workspace: 42,
            user_public_key: [3; 32],
            payload: PersonalFabricPayload::NearbyTransfer(PersonalNearbyTransferFrame::Chunk {
                transfer_id: [4; 16],
                request_id: [5; 16],
                offset: 99,
                bytes: vec![7; PERSONAL_TRANSFER_CHUNK_BYTES],
            }),
        };
        let encoded = postcard::to_allocvec(&wire).unwrap();
        assert!(encoded.len() < crate::shell::device_fabric::MAX_FABRIC_CHECKPOINT_BYTES);
        let decoded: PersonalFabricWireFrame = postcard::from_bytes(&encoded).unwrap();
        let PersonalFabricPayload::NearbyTransfer(PersonalNearbyTransferFrame::Chunk {
            transfer_id,
            request_id,
            offset,
            bytes,
        }) = decoded.payload
        else {
            panic!("wrong nearby transfer frame");
        };
        assert_eq!(transfer_id, [4; 16]);
        assert_eq!(request_id, [5; 16]);
        assert_eq!(offset, 99);
        assert_eq!(bytes.len(), PERSONAL_TRANSFER_CHUNK_BYTES);
    }
}

use iroh::endpoint::presets;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
use tokio::time::timeout;

async fn endpoint() -> (Endpoint, MemoryLookup) {
    let lookup = MemoryLookup::new();
    let endpoint = Endpoint::builder(presets::N0)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .unwrap();
    (endpoint, lookup)
}

/// Reads personal fabric frames on every connection it accepts, as the
/// actor does on each gossip connection it is handed.
#[derive(Debug, Clone)]
struct FabricReader(mpsc::Sender<InternalEvent>);

impl ProtocolHandler for FabricReader {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        read_personal_fabric(connection, self.0.clone(), &AtomicU64::new(1));
        Ok(())
    }
}

/// Two of a person's devices on their personal topic `ws`, which travels
/// directly between them over gossip, the first dialling the second, once
/// each has seen the other join.
async fn linked_pair(
    ws: WorkspaceId,
    a: (Endpoint, MemoryLookup),
    b: (Endpoint, MemoryLookup),
) -> [(
    mpsc::Sender<NetCmd>,
    mpsc::Receiver<NetEvent>,
    tokio::task::JoinHandle<()>,
); 2] {
    let (a_id, b_id) = (a.0.id(), b.0.id());
    let b_addr = b.0.addr();
    let mut pair = [a, b].map(|(endpoint, lookup)| {
        let (cmd, task) = Actor::spawn(endpoint, lookup);
        let (events, rx) = mpsc::channel(64);
        (cmd, rx, task, events)
    });
    for (cmd, _, _, events) in &pair {
        cmd.send(NetCmd::JoinPersonalTopic {
            ws,
            on_event: events.clone(),
        })
        .await
        .unwrap();
    }
    pair[0]
        .0
        .send(NetCmd::Connect { addr: b_addr })
        .await
        .unwrap();
    for ((_, rx, _, _), other) in pair.iter_mut().zip([b_id, a_id]) {
        wait_for_event(
            "the pair links",
            rx,
            |event| matches!(event, NetEvent::PeerJoined { peer, .. } if *peer == other),
        )
        .await;
    }
    pair.map(|(cmd, rx, task, _)| (cmd, rx, task))
}

async fn wait_for_event(
    what: &str,
    rx: &mut mpsc::Receiver<NetEvent>,
    wanted: impl Fn(&NetEvent) -> bool + Send + Sync,
) {
    tilo_test_env::within(what, async {
        loop {
            if wanted(&rx.recv().await.expect("the actor is running")) {
                return;
            }
        }
    })
    .await;
}

fn fabric_action_from(event: &NetEvent, from: EndpointId, action: &[u8]) -> bool {
    matches!(
        event,
        NetEvent::PersonalFabric {
            peer,
            payload: PersonalFabricPayload::Action(bytes),
            ..
        } if *peer == from && bytes == action
    )
}

/// Connections `endpoint` holds open.
fn open_connections(endpoint: &Endpoint) -> u64 {
    let socket = &endpoint.metrics().socket;
    socket
        .num_conns_opened
        .get()
        .saturating_sub(socket.num_conns_closed.get())
}

/// Two linked devices keep one connection between them: personal fabric
/// frames, both ways, share the one gossip runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_linked_pair_keeps_one_connection_for_gossip_and_personal_fabric() {
    let (a, lookup_a) = endpoint().await;
    let (b, lookup_b) = endpoint().await;
    let (a_id, b_id) = (a.id(), b.id());
    let [(cmd_a, mut rx_a, _task_a), (cmd_b, mut rx_b, _task_b)] =
        linked_pair(45, (a.clone(), lookup_a), (b.clone(), lookup_b)).await;

    for (cmd, target) in [(&cmd_a, b_id), (&cmd_b, a_id)] {
        cmd.send(NetCmd::PublishPersonalFabricTarget {
            ws: 45,
            user_public_key: [0xB5; 32],
            target,
            payload: PersonalFabricPayload::Action(vec![5, 5]),
        })
        .await
        .unwrap();
    }
    wait_for_event("A's fabric frame at B", &mut rx_b, |event| {
        fabric_action_from(event, a_id, &[5, 5])
    })
    .await;
    wait_for_event("B's fabric frame at A", &mut rx_a, |event| {
        fabric_action_from(event, b_id, &[5, 5])
    })
    .await;
    cmd_a
        .send(NetCmd::PublishPersonalFabric {
            ws: 45,
            user_public_key: [0xB5; 32],
            payload: PersonalFabricPayload::Request,
        })
        .await
        .unwrap();
    wait_for_event("A's fabric request at B", &mut rx_b, |event| {
        matches!(
            event,
            NetEvent::PersonalFabric {
                payload: PersonalFabricPayload::Request,
                ..
            }
        )
    })
    .await;

    for (name, endpoint) in [("A", &a), ("B", &b)] {
        assert_eq!(
            open_connections(endpoint),
            1,
            "{name} holds exactly one connection to its sibling"
        );
        assert_eq!(
            endpoint.metrics().socket.num_conns_opened.get(),
            1,
            "{name} never opened a second connection"
        );
    }
}

/// Join the personal topic `ws` on the actor `cmd` drives, and hear its
/// events.
async fn joined(cmd: &mpsc::Sender<NetCmd>, ws: WorkspaceId) -> mpsc::Receiver<NetEvent> {
    let (on_event, events) = mpsc::channel(64);
    cmd.send(NetCmd::JoinPersonalTopic { ws, on_event })
        .await
        .unwrap();
    events
}

/// A device that restarts on the same key hears its sibling's fabric at
/// once: the sibling answers on the connection the restarted device just
/// reached it on, before the membership of its old run has lapsed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restarted_device_hears_its_siblings_fabric_at_once() {
    let b_key = ed25519_dalek::SigningKey::from_bytes(&[0x5B; 32]);
    let a = endpoint().await;
    let a_addr = a.0.addr();
    let a_id = a.0.id();
    let b = identified_endpoint(&b_key).await;
    let b_id = b.0.id();
    let [(cmd_a, mut rx_a, _task_a), (cmd_b, _rx_b, task_b)] = linked_pair(46, a, b).await;
    // B's first run ends: its actor stops and its endpoint closes.
    drop(cmd_b);
    task_b.await.unwrap();

    let (b_restarted, lookup_restarted) = identified_endpoint(&b_key).await;
    let (cmd_restarted, _task_restarted) = Actor::spawn(b_restarted, lookup_restarted);
    let mut rx_restarted = joined(&cmd_restarted, 46).await;
    cmd_restarted
        .send(NetCmd::Connect { addr: a_addr })
        .await
        .unwrap();
    wait_for_event(
        "the restarted device links",
        &mut rx_restarted,
        |event| matches!(event, NetEvent::PeerJoined { peer, .. } if *peer == a_id),
    )
    .await;

    let started = Instant::now();
    cmd_restarted
        .send(NetCmd::PublishPersonalFabric {
            ws: 46,
            user_public_key: [0xB6; 32],
            payload: PersonalFabricPayload::Request,
        })
        .await
        .unwrap();
    wait_for_event("the restarted device's request", &mut rx_a, |event| {
        matches!(
            event,
            NetEvent::PersonalFabric {
                peer,
                payload: PersonalFabricPayload::Request,
                ..
            } if *peer == b_id
        )
    })
    .await;
    cmd_a
        .send(NetCmd::PublishPersonalFabricTarget {
            ws: 46,
            user_public_key: [0xB6; 32],
            target: b_id,
            payload: PersonalFabricPayload::Action(vec![6, 6]),
        })
        .await
        .unwrap();
    wait_for_event("the sibling's answer", &mut rx_restarted, |event| {
        fabric_action_from(event, a_id, &[6, 6])
    })
    .await;
    assert!(
        started.elapsed() < PEER_TIMEOUT / 3,
        "the answer took {:?}, as long as a stale connection lasts",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn personal_fabric_unknown_extension_preserves_following_streams() {
    const FIXTURE_ALPN: &[u8] = b"tilo-test-fabric-reader";
    let (a, _) = endpoint().await;
    let (b, _) = endpoint().await;
    let (internal_tx, mut events) = mpsc::channel(8);
    let router = Router::builder(b.clone())
        .accept(FIXTURE_ALPN, FabricReader(internal_tx))
        .spawn();
    let connection = a.connect(b.addr(), FIXTURE_ALPN).await.unwrap();
    let frame = PersonalFabricWireFrame {
        workspace: 43,
        user_public_key: [0xA5; 32],
        payload: PersonalFabricPayload::Snapshot(vec![1, 2, 3]),
    };
    let valid = postcard::to_allocvec(&frame).unwrap();
    assert_eq!(valid[33], 1, "the legacy Snapshot payload tag stays fixed");
    let mut future = valid.clone();
    future[33] = 9; // First unsupported tag after optional connector observations.
    assert!(postcard::from_bytes::<PersonalFabricWireFrame>(&future).is_err());
    for bytes in [&future, &valid] {
        let (mut stream, _) = connection.open_bi().await.unwrap();
        stream.write_all(bytes).await.unwrap();
        stream.finish().unwrap();
    }
    let event = tilo_test_env::within("valid checkpoint after unknown extension", events.recv())
        .await
        .unwrap();
    assert!(matches!(event, InternalEvent::PersonalFabric {
        ws: 43, payload: PersonalFabricPayload::Snapshot(bytes), ..
    } if bytes == vec![1, 2, 3]));
    connection.close(0u32.into(), b"fixture complete");
    router.shutdown().await.unwrap();
    a.close().await;
}

#[tokio::test]
async fn remote_input_bypasses_a_saturated_surface_frame_queue() {
    let (endpoint, _lookup) = endpoint().await;
    let peer = endpoint.id();
    let (media_tx, mut media_rx) = mpsc::channel(1);
    let (input_tx, mut input_rx) = mpsc::channel(1);
    media_tx
        .send(InternalEvent::Closed { ws: 7 })
        .await
        .unwrap();

    let queued = enqueue_surface_event(
        &media_tx,
        &input_tx,
        InternalEvent::SurfaceFrame {
            ws: 7,
            peer,
            connection_incarnation: 1,
            fingerprint: [0; 32],
            msg: NeighbourMsg::RemoteInput {
                peer: 9,
                surface: 11,
                event: vec![1],
            },
        },
    )
    .await;

    assert!(queued);
    assert!(matches!(
        input_rx.try_recv(),
        Ok(InternalEvent::SurfaceFrame {
            msg: NeighbourMsg::RemoteInput { surface: 11, .. },
            ..
        })
    ));
    assert!(matches!(
        media_rx.try_recv(),
        Ok(InternalEvent::Closed { ws: 7 })
    ));
}

async fn identified_endpoint(key: &ed25519_dalek::SigningKey) -> (Endpoint, MemoryLookup) {
    let lookup = MemoryLookup::new();
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .address_lookup(lookup.clone())
        .bind()
        .await
        .unwrap();
    (endpoint, lookup)
}

fn enrol(
    root: &ed25519_dalek::SigningKey,
    capabilities: DeviceCapabilities,
    label: &str,
) -> (ed25519_dalek::SigningKey, DeviceCert) {
    let key = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn two_actors_exchange_and_announce_identity_over_iroh() {
    // Phase `two_actors_exchange_over_iroh`.
    {
        let (a, lookup_a) = endpoint().await;
        let (b, lookup_b) = endpoint().await;
        let source_peer = crate::network::behaviour::peer_liveness_handle(&a.id());
        let b_addr = b.addr();
        let (cmd_a, _a_task) = Actor::spawn(a, lookup_a);
        let (cmd_b, _b_task) = Actor::spawn(b, lookup_b);
        let (event_a, mut rx_a) = mpsc::channel(16);
        let (event_b, mut rx_b) = mpsc::channel(16);
        cmd_a
            .send(NetCmd::JoinPersonalTopic {
                ws: 42,
                on_event: event_a,
            })
            .await
            .unwrap();
        cmd_b
            .send(NetCmd::JoinPersonalTopic {
                ws: 42,
                on_event: event_b,
            })
            .await
            .unwrap();
        cmd_a.send(NetCmd::Connect { addr: b_addr }).await.unwrap();
        loop {
            let event = tilo_test_env::within("a message on rx_a", rx_a.recv())
                .await
                .unwrap();
            if matches!(event, NetEvent::PeerJoined { .. }) {
                break;
            }
        }
        cmd_b
            .send(NetCmd::Publish {
                at: tokio::time::Instant::now(),
                ws: 42,
                msg: NeighbourMsg::SyncReq {
                    since: vec![],
                    room: String::new(),
                    invite_id: None,
                    nonce: 0,
                },
            })
            .await
            .unwrap();
        loop {
            let event = tilo_test_env::within("a message on rx_a", rx_a.recv())
                .await
                .unwrap();
            if matches!(
                event,
                NetEvent::Msg {
                    msg: NeighbourMsg::SyncReq { .. },
                    ..
                }
            ) {
                break;
            }
        }

        let dedicated_before =
            DEDICATED_SURFACE_FRAMES_RECEIVED.load(std::sync::atomic::Ordering::Relaxed);
        cmd_a
            .send(NetCmd::Publish {
                at: tokio::time::Instant::now(),
                ws: 42,
                msg: NeighbourMsg::SurfaceFrame {
                    surface: 7,
                    source_peer,
                    encoded_h264: vec![0, 0, 0, 1, 0x65],
                    ts: 99,
                    seq: 0,
                },
            })
            .await
            .unwrap();
        loop {
            let event = tilo_test_env::within("a message on rx_b", rx_b.recv())
                .await
                .unwrap();
            if matches!(
                event,
                NetEvent::Msg {
                    msg: NeighbourMsg::SurfaceFrame {
                        surface: 7,
                        ts: 99,
                        ..
                    },
                    ..
                }
            ) {
                break;
            }
        }
        assert!(
            DEDICATED_SURFACE_FRAMES_RECEIVED.load(std::sync::atomic::Ordering::Relaxed)
                > dedicated_before,
            "surface frame should arrive through the dedicated QUIC ALPN"
        );
    }
    // Phase `actors_announce_identity_when_a_neighbor_connects`.
    {
        let root_a = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let root_b = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let (key_a, cert_a) = enrol(&root_a, DeviceCapabilities::primary(), "a");
        let (key_b, cert_b) = enrol(&root_b, DeviceCapabilities::primary(), "b");
        let expected_a = cert_a.device_public_key;
        let expected_b = cert_b.device_public_key;
        let (endpoint_a, lookup_a) = identified_endpoint(&key_a).await;
        let (endpoint_b, lookup_b) = identified_endpoint(&key_b).await;
        let addr_b = endpoint_b.addr();
        let identity_a = HostIdentity {
            device_cert: cert_a,
            device_signing_key: key_a,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let identity_b = HostIdentity {
            device_cert: cert_b,
            device_signing_key: key_b,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let (cmd_a, _task_a) = Actor::spawn_reporting_addrs(
            endpoint_a,
            lookup_a,
            Some(identity_a),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (cmd_b, _task_b) = Actor::spawn_reporting_addrs(
            endpoint_b,
            lookup_b,
            Some(identity_b),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (events_a, mut rx_a) = mpsc::channel(16);
        let (events_b, mut rx_b) = mpsc::channel(16);
        cmd_a
            .send(NetCmd::JoinPersonalTopic {
                ws: 42,
                on_event: events_a,
            })
            .await
            .unwrap();
        cmd_b
            .send(NetCmd::JoinPersonalTopic {
                ws: 42,
                on_event: events_b,
            })
            .await
            .unwrap();
        cmd_a.send(NetCmd::Connect { addr: addr_b }).await.unwrap();

        for (rx, expected) in [(&mut rx_a, expected_b), (&mut rx_b, expected_a)] {
            tilo_test_env::within(
                "identity announcement should not wait for the heartbeat",
                async {
                    loop {
                        if let Some(NetEvent::Msg {
                            msg: NeighbourMsg::IdentityHello { cert, .. },
                            ..
                        }) = rx.recv().await
                            && cert.device_public_key == expected
                        {
                            break;
                        }
                    }
                },
            )
            .await;
        }
    }
}

/// What this actor's peers delivered to it so far, by kind.
async fn tally_of(cmd: &mpsc::Sender<NetCmd>) -> std::collections::BTreeMap<&'static str, u64> {
    let (reply, answer) = tokio::sync::oneshot::channel();
    cmd.send(NetCmd::WireTally(reply)).await.unwrap();
    answer.await.unwrap().snapshot()
}

/// An identified actor that runs its upkeep tick every `tick`, joined
/// to workspace 42.
async fn idle_test_actor(
    root: &ed25519_dalek::SigningKey,
    label: &str,
    tick: Duration,
) -> (
    mpsc::Sender<NetCmd>,
    mpsc::Receiver<NetEvent>,
    EndpointAddr,
    tokio::task::JoinHandle<()>,
) {
    let (key, cert) = enrol(root, DeviceCapabilities::primary(), label);
    let (endpoint, lookup) = identified_endpoint(&key).await;
    let addr = endpoint.addr();
    let identity = HostIdentity {
        device_cert: cert,
        device_signing_key: key,
        workspace_device_certs: HashMap::new(),
        workspace_revocations: HashMap::new(),
        identity_paths: None,
    };
    let (cmd, task) = Actor::spawn_with_heartbeat(
        endpoint,
        lookup,
        Some(identity),
        Arc::new(Mutex::new(Vec::new())),
        tick,
        Reachability::default(),
        None,
    );
    let (events, rx) = mpsc::channel(64);
    cmd.send(NetCmd::JoinPersonalTopic {
        ws: 42,
        on_event: events,
    })
    .await
    .unwrap();
    (cmd, rx, addr, task)
}

/// Wait on `rx` until the neighbour has said who it is and what it can
/// decode, as it does once when the link comes up.
async fn await_identity_and_codecs(rx: &mut mpsc::Receiver<NetEvent>) {
    let (mut identity, mut codecs) = (false, false);
    while !(identity && codecs) {
        match tilo_test_env::within("the neighbour's identity and codecs", rx.recv())
            .await
            .unwrap()
        {
            NetEvent::Msg {
                msg: NeighbourMsg::IdentityHello { .. },
                ..
            } => identity = true,
            NetEvent::Msg {
                msg: NeighbourMsg::MediaCapabilities { .. },
                ..
            } => codecs = true,
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn linked_actors_keep_liveness_on_the_transport_and_say_nothing_while_idle() {
    // The upkeep tick runs every 50 ms here, a hundred times its pace in
    // a session, so a second of idling is a hundred ticks.
    const TICK: Duration = Duration::from_millis(50);
    let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
    let (cmd_a, mut rx_a, _, task_a) = idle_test_actor(&root, "a", TICK).await;
    let (cmd_b, mut rx_b, addr_b, task_b) = idle_test_actor(&root, "b", TICK).await;
    cmd_a.send(NetCmd::Connect { addr: addr_b }).await.unwrap();

    // Phase `a_new_neighbour_hears_identity_and_codecs_once_per_link`.
    await_identity_and_codecs(&mut rx_a).await;
    await_identity_and_codecs(&mut rx_b).await;

    // Phase `a_linked_idle_pair_sends_no_application_message`. Before
    // the transport carried liveness every tick sent a heartbeat, an
    // identity hello and a codec advert per topic.
    tokio::time::sleep(TICK * 4).await;
    let (before_a, before_b) = (tally_of(&cmd_a).await, tally_of(&cmd_b).await);
    tokio::time::sleep(TICK * 20).await;
    assert_eq!(
        tally_of(&cmd_a).await,
        before_a,
        "a heard nothing while idle"
    );
    assert_eq!(
        tally_of(&cmd_b).await,
        before_b,
        "b heard nothing while idle"
    );
    assert!(
        std::iter::from_fn(|| rx_a.try_recv().ok())
            .chain(std::iter::from_fn(|| rx_b.try_recv().ok()))
            .all(|event| !matches!(event, NetEvent::PeerLeft { .. })),
        "a silent, linked peer stays a member"
    );

    // Phase `a_departed_peer_is_dropped_within_the_departure_grace`. A
    // peer that shuts down closes its links; it is dropped that grace
    // later, not after a heartbeat timeout.
    drop(cmd_b);
    task_b.await.unwrap();
    let gone = Instant::now();
    let left = tilo_test_env::within("the departed peer to be dropped", async {
        loop {
            if let Some(NetEvent::PeerLeft { cause, .. }) = rx_a.recv().await {
                return cause;
            }
        }
    })
    .await;
    assert_eq!(left, PeerDepartureCause::LivenessTimedOut);
    let took = gone.elapsed();
    assert!(
        took <= DEPARTURE_GRACE + Duration::from_secs(3),
        "dropped {took:?} after it left"
    );
    drop(rx_b);
    drop(cmd_a);
    task_a.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn direct_collaboration_traffic_reaches_only_its_exact_target() {
    // Phase `dead_surface_viewer_does_not_stall_later_collaboration_messages`.
    {
        let (a, lookup_a) = endpoint().await;
        let (b, lookup_b) = endpoint().await;
        let (c, lookup_c) = endpoint().await;
        let source_peer = crate::network::behaviour::peer_liveness_handle(&a.id());
        let b_addr = b.addr();
        let c_addr = c.addr();
        let (cmd_a, task_a) = Actor::spawn(a, lookup_a);
        let (cmd_b, task_b) = Actor::spawn(b, lookup_b);
        let (cmd_c, task_c) = Actor::spawn(c, lookup_c);
        let (event_a, mut rx_a) = mpsc::channel(32);
        let (event_b, _rx_b) = mpsc::channel(32);
        let (event_c, mut rx_c) = mpsc::channel(32);
        for (command, events) in [(&cmd_a, event_a), (&cmd_b, event_b), (&cmd_c, event_c)] {
            command
                .send(NetCmd::JoinPersonalTopic {
                    ws: 44,
                    on_event: events,
                })
                .await
                .unwrap();
        }
        cmd_a.send(NetCmd::Connect { addr: b_addr }).await.unwrap();
        cmd_a.send(NetCmd::Connect { addr: c_addr }).await.unwrap();
        let mut joined = HashSet::new();
        while joined.len() < 2 {
            if let NetEvent::PeerJoined { peer, .. } =
                tilo_test_env::within("a message on rx_a", rx_a.recv())
                    .await
                    .unwrap()
            {
                joined.insert(peer);
            }
        }

        // Leave A's authenticated membership evidence intact while making B's
        // dedicated media address unreachable. A direct dial may consume the
        // full media timeout, but it must happen outside the actor loop.
        task_b.abort();
        let _ = task_b.await;
        tokio::time::sleep(Duration::from_millis(25)).await;
        // A dial to the dead viewer takes up to its 300 ms timeout. Inside
        // the actor loop every attempt would wait it out; outside it, an
        // attempt on a loaded runner is late only now and then. Each
        // attempt names a new surface, so each dials the viewer anew.
        let mut fastest = Duration::MAX;
        for attempt in 0..5_u32 {
            let started = Instant::now();
            cmd_a
                .send(NetCmd::Publish {
                    at: tokio::time::Instant::now(),
                    ws: 44,
                    msg: NeighbourMsg::SurfaceFrame {
                        surface: 9 + u64::from(attempt),
                        source_peer,
                        encoded_h264: vec![0, 0, 0, 1, 0x65],
                        ts: 1,
                        seq: 0,
                    },
                })
                .await
                .unwrap();
            cmd_a
                .send(NetCmd::Publish {
                    at: tokio::time::Instant::now(),
                    ws: 44,
                    msg: NeighbourMsg::SyncReq {
                        since: vec![],
                        room: "responsive".into(),
                        invite_id: None,
                        nonce: u64::from(attempt),
                    },
                })
                .await
                .unwrap();
            loop {
                let event = tilo_test_env::within(
                    "dead media viewer stalled the collaboration actor",
                    rx_c.recv(),
                )
                .await
                .unwrap();
                if matches!(
                    event,
                    NetEvent::Msg {
                        msg: NeighbourMsg::SyncReq { ref room, .. },
                        ..
                    } if room == "responsive"
                ) {
                    break;
                }
            }
            fastest = fastest.min(started.elapsed());
        }
        assert!(
            fastest < Duration::from_millis(250),
            "every attempt waited for the dead viewer's dial: {fastest:?}"
        );
        task_a.abort();
        task_c.abort();
    }
    // Phase `personal_fabric_action_reaches_only_the_exact_target`.
    {
        let (a, lookup_a) = endpoint().await;
        let (b, lookup_b) = endpoint().await;
        let (c, lookup_c) = endpoint().await;
        let a_id = a.id();
        let b_id = b.id();
        let c_id = c.id();
        let b_addr = b.addr();
        let c_addr = c.addr();
        let (cmd_a, _a_task) = Actor::spawn(a, lookup_a);
        let (cmd_b, _b_task) = Actor::spawn(b, lookup_b);
        let (cmd_c, _c_task) = Actor::spawn(c, lookup_c);
        let (event_a, mut rx_a) = mpsc::channel(16);
        let (event_b, mut rx_b) = mpsc::channel(16);
        let (event_c, mut rx_c) = mpsc::channel(16);
        for (cmd, events) in [(&cmd_a, event_a), (&cmd_b, event_b), (&cmd_c, event_c)] {
            cmd.send(NetCmd::JoinPersonalTopic {
                ws: 44,
                on_event: events,
            })
            .await
            .unwrap();
        }
        cmd_a.send(NetCmd::Connect { addr: b_addr }).await.unwrap();
        cmd_a.send(NetCmd::Connect { addr: c_addr }).await.unwrap();

        let mut a_saw_b = false;
        let mut a_saw_c = false;
        while !(a_saw_b && a_saw_c) {
            match tilo_test_env::within("a message on rx_a", rx_a.recv())
                .await
                .unwrap()
            {
                NetEvent::PeerJoined { peer, .. } if peer == b_id => a_saw_b = true,
                NetEvent::PeerJoined { peer, .. } if peer == c_id => a_saw_c = true,
                _ => {}
            }
        }
        for receiver in [&mut rx_b, &mut rx_c] {
            loop {
                let event = tilo_test_env::within("a message on receiver", receiver.recv())
                    .await
                    .unwrap();
                if matches!(event, NetEvent::PeerJoined { peer, .. } if peer == a_id) {
                    break;
                }
            }
        }

        cmd_a
            .send(NetCmd::PublishPersonalFabricTarget {
                ws: 44,
                user_public_key: [0xB4; 32],
                target: b_id,
                payload: PersonalFabricPayload::Action(vec![4, 4]),
            })
            .await
            .unwrap();
        loop {
            let event = tilo_test_env::within("a message on rx_b", rx_b.recv())
                .await
                .unwrap();
            if matches!(
                event,
                NetEvent::PersonalFabric {
                    payload: PersonalFabricPayload::Action(bytes),
                    ..
                } if bytes == vec![4, 4]
            ) {
                break;
            }
        }
        while let Ok(Some(event)) = timeout(Duration::from_millis(500), rx_c.recv()).await {
            assert!(
                !matches!(
                    event,
                    NetEvent::PersonalFabric {
                        payload: PersonalFabricPayload::Action(_),
                        ..
                    }
                ),
                "personal action leaked to a non-target device"
            );
        }
    }
    // Phase `direct_workspace_control_input_reaches_only_the_exact_target`.
    {
        let (a, lookup_a) = endpoint().await;
        let (b, lookup_b) = endpoint().await;
        let (c, lookup_c) = endpoint().await;
        let a_id = a.id();
        let b_id = b.id();
        let c_id = c.id();
        let b_addr = b.addr();
        let c_addr = c.addr();
        let (cmd_a, _a_task) = Actor::spawn(a, lookup_a);
        let (cmd_b, _b_task) = Actor::spawn(b, lookup_b);
        let (cmd_c, _c_task) = Actor::spawn(c, lookup_c);
        let (event_a, mut rx_a) = mpsc::channel(16);
        let (event_b, mut rx_b) = mpsc::channel(16);
        let (event_c, mut rx_c) = mpsc::channel(16);
        for (cmd, events) in [(&cmd_a, event_a), (&cmd_b, event_b), (&cmd_c, event_c)] {
            cmd.send(NetCmd::JoinPersonalTopic {
                ws: 43,
                on_event: events,
            })
            .await
            .unwrap();
        }
        cmd_a.send(NetCmd::Connect { addr: b_addr }).await.unwrap();
        cmd_a.send(NetCmd::Connect { addr: c_addr }).await.unwrap();

        let mut a_saw_b = false;
        let mut a_saw_c = false;
        while !(a_saw_b && a_saw_c) {
            match tilo_test_env::within("a message on rx_a", rx_a.recv())
                .await
                .unwrap()
            {
                NetEvent::PeerJoined { peer, .. } if peer == b_id => a_saw_b = true,
                NetEvent::PeerJoined { peer, .. } if peer == c_id => a_saw_c = true,
                _ => {}
            }
        }
        for receiver in [&mut rx_b, &mut rx_c] {
            loop {
                let event = tilo_test_env::within("a message on receiver", receiver.recv())
                    .await
                    .unwrap();
                if matches!(event, NetEvent::PeerJoined { peer, .. } if peer == a_id) {
                    break;
                }
            }
        }

        for _ in 0..2 {
            cmd_a
                .send(NetCmd::PublishWorkspaceControl {
                    ws: 43,
                    msg: NeighbourMsg::RemoteInput {
                        peer: crate::network::behaviour::peer_liveness_handle(&a_id),
                        surface: 7,
                        event: vec![0x51, 0x4f, 0x53],
                    },
                    route: WorkspaceControlRoute::Direct { target: b_id },
                })
                .await
                .unwrap();
        }

        let mut delivered = 0;
        while delivered < 2 {
            let event = tilo_test_env::within("a message on rx_b", rx_b.recv())
                .await
                .unwrap();
            if matches!(
                event,
                NetEvent::Msg {
                    msg: NeighbourMsg::RemoteInput { surface: 7, .. },
                    ..
                }
            ) {
                delivered += 1;
            }
        }
        while let Ok(Some(event)) = timeout(Duration::from_millis(500), rx_c.recv()).await {
            assert!(
                !matches!(
                    event,
                    NetEvent::Msg {
                        msg: NeighbourMsg::RemoteInput { .. },
                        ..
                    }
                ),
                "direct input leaked to a non-target workspace peer"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "the integration test verifies both receiver framing and the sender's ordered progress lifecycle"
)]
async fn personal_fabric_reaches_members_and_streams_verified_media() {
    // Phase `personal_fabric_reaches_the_topics_member`.
    {
        let (a, lookup_a) = endpoint().await;
        let (b, lookup_b) = endpoint().await;
        let b_addr = b.addr();
        let (cmd_a, _a_task) = Actor::spawn(a, lookup_a);
        let (cmd_b, _b_task) = Actor::spawn(b, lookup_b);
        let (event_a, mut rx_a) = mpsc::channel(16);
        let (event_b, mut rx_b) = mpsc::channel(16);
        cmd_a
            .send(NetCmd::JoinPersonalTopic {
                ws: 43,
                on_event: event_a,
            })
            .await
            .unwrap();
        cmd_b
            .send(NetCmd::JoinPersonalTopic {
                ws: 43,
                on_event: event_b,
            })
            .await
            .unwrap();
        cmd_a.send(NetCmd::Connect { addr: b_addr }).await.unwrap();
        while !matches!(
            tilo_test_env::within("a message on rx_a", rx_a.recv())
                .await
                .unwrap(),
            NetEvent::PeerJoined { .. }
        ) {}

        cmd_a
            .send(NetCmd::PublishPersonalFabric {
                ws: 43,
                user_public_key: [0xA5; 32],
                payload: PersonalFabricPayload::Snapshot(vec![1, 2, 3]),
            })
            .await
            .unwrap();
        loop {
            let event = tilo_test_env::within("a message on rx_b", rx_b.recv())
                .await
                .unwrap();
            if matches!(
                event,
                NetEvent::PersonalFabric {
                    ws: 43,
                    user_public_key,
                    payload: PersonalFabricPayload::Snapshot(bytes),
                    ..
                } if user_public_key == [0xA5; 32] && bytes == vec![1, 2, 3]
            ) {
                break;
            }
        }
    }
    // Phase `personal_transfer_streams_in_order_with_a_verified_finish_hash`.
    {
        let (a, lookup_a) = endpoint().await;
        let (b, lookup_b) = endpoint().await;
        let a_id = a.id();
        let b_id = b.id();
        let b_addr = b.addr();
        let (cmd_a, _a_task) = Actor::spawn(a, lookup_a);
        let (cmd_b, _b_task) = Actor::spawn(b, lookup_b);
        // Progress is best-effort; leave room for every chunk's report
        // while the receiver side is drained first.
        let (event_a, mut rx_a) = mpsc::channel(256);
        let (event_b, mut rx_b) = mpsc::channel(256);
        for (command, events) in [(&cmd_a, event_a), (&cmd_b, event_b)] {
            command
                .send(NetCmd::JoinPersonalTopic {
                    ws: 45,
                    on_event: events,
                })
                .await
                .unwrap();
        }
        cmd_a.send(NetCmd::Connect { addr: b_addr }).await.unwrap();
        while !matches!(
            tilo_test_env::within("a message on rx_a", rx_a.recv()).await
                .unwrap(),
            NetEvent::PeerJoined { peer, .. } if peer == b_id
        ) {}
        while !matches!(
            tilo_test_env::within("a message on rx_b", rx_b.recv()).await
                .unwrap(),
            NetEvent::PeerJoined { peer, .. } if peer == a_id
        ) {}

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private-host-name.flac");
        let bytes = vec![0x9a; PERSONAL_TRANSFER_CHUNK_BYTES.saturating_add(31)];
        std::fs::write(&path, &bytes).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let transfer_id = [0x45; 16];
        let request_id = [0x46; 16];
        cmd_a
            .send(NetCmd::PublishNearbyTransfer {
                ws: 45,
                user_public_key: [0xA6; 32],
                target: b_id,
                transfer: NearbyTransferCheckpoint {
                    transfer_id,
                    request_id,
                    total_bytes: u64::try_from(bytes.len()).unwrap(),
                    resume_offset: 0,
                    prefix_hash: *blake3::hash(&[]).as_bytes(),
                    content_hash: *blake3::hash(&bytes).as_bytes(),
                },
                file,
            })
            .await
            .unwrap();

        let mut received = Vec::new();
        loop {
            let event = tilo_test_env::within("a message on rx_b", rx_b.recv())
                .await
                .unwrap();
            let NetEvent::PersonalFabric {
                payload: PersonalFabricPayload::NearbyTransfer(frame),
                ..
            } = event
            else {
                continue;
            };
            match frame {
                PersonalNearbyTransferFrame::Chunk {
                    transfer_id: id,
                    request_id: request,
                    offset,
                    bytes: chunk,
                } => {
                    assert_eq!((id, request), (transfer_id, request_id));
                    assert_eq!(offset, u64::try_from(received.len()).unwrap());
                    received.extend_from_slice(&chunk);
                }
                PersonalNearbyTransferFrame::Finish {
                    transfer_id: id,
                    request_id: request,
                    content_hash,
                } => {
                    assert_eq!((id, request), (transfer_id, request_id));
                    assert_eq!(content_hash, *blake3::hash(&received).as_bytes());
                    break;
                }
                PersonalNearbyTransferFrame::Abort { .. } => panic!("transfer aborted"),
            }
        }
        assert_eq!(received, bytes);

        let mut statuses = Vec::new();
        loop {
            let event = tilo_test_env::within("a message on rx_a", rx_a.recv())
                .await
                .unwrap();
            let NetEvent::NearbyTransferProgress {
                target,
                transfer_id: id,
                sent_bytes,
                total_bytes,
                status,
            } = event
            else {
                continue;
            };
            assert_eq!(target, b_id);
            assert_eq!(id, transfer_id);
            assert!(sent_bytes <= total_bytes);
            statuses.push((status, sent_bytes));
            if status == PersonalTransferStatus::Completed {
                break;
            }
        }
        assert_eq!(
            statuses.first(),
            Some(&(PersonalTransferStatus::Started, 0))
        );
        assert!(statuses.iter().any(|(status, sent)| {
            *status == PersonalTransferStatus::Progress
                && *sent == u64::try_from(bytes.len()).unwrap()
        }));
        assert_eq!(
            statuses.last(),
            Some(&(
                PersonalTransferStatus::Completed,
                u64::try_from(bytes.len()).unwrap()
            ))
        );

        // Phase `personal_transfer_cancel_stops_only_the_exact_stream`.
        let large = vec![0x3c; PERSONAL_TRANSFER_CHUNK_BYTES.saturating_mul(64)];
        let large_path = dir.path().join("film.mkv");
        std::fs::write(&large_path, &large).unwrap();
        let cancelled_id = [0x47; 16];
        cmd_a
            .send(NetCmd::PublishNearbyTransfer {
                ws: 45,
                user_public_key: [0xA6; 32],
                target: b_id,
                transfer: NearbyTransferCheckpoint {
                    transfer_id: cancelled_id,
                    request_id,
                    total_bytes: u64::try_from(large.len()).unwrap(),
                    resume_offset: 0,
                    prefix_hash: *blake3::hash(&[]).as_bytes(),
                    content_hash: *blake3::hash(&large).as_bytes(),
                },
                file: std::fs::File::open(&large_path).unwrap(),
            })
            .await
            .unwrap();
        cmd_a
            .send(NetCmd::CancelNearbyTransfer {
                target: a_id,
                transfer_id: cancelled_id,
            })
            .await
            .unwrap();
        cmd_a
            .send(NetCmd::CancelNearbyTransfer {
                target: b_id,
                transfer_id: cancelled_id,
            })
            .await
            .unwrap();
        loop {
            let event = tilo_test_env::within("a message on rx_a", rx_a.recv())
                .await
                .unwrap();
            let NetEvent::NearbyTransferProgress {
                target,
                transfer_id: id,
                status,
                ..
            } = event
            else {
                continue;
            };
            if id != cancelled_id {
                // Late progress of the finished stream.
                continue;
            }
            assert_eq!(target, b_id);
            assert_ne!(
                status,
                PersonalTransferStatus::Completed,
                "a cancelled stream never completes"
            );
            if status == PersonalTransferStatus::Cancelled {
                break;
            }
        }
    }
}
