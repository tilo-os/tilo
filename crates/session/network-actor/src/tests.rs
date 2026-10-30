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
    // Phase `nearby_transfer_frame_round_trips_exact_binding_and_bounded_chunk`.
    {
        let wire = PersonalFabricWireFrame {
            user_public_key: [3; 32],
            payload: PersonalFabricPayload::NearbyTransfer(PersonalNearbyTransferFrame::Chunk {
                transfer_id: [4; 16],
                request_id: [5; 16],
                offset: 99,
                bytes: vec![7; PERSONAL_TRANSFER_CHUNK_BYTES],
            }),
        };
        let encoded = postcard::to_allocvec(&wire).unwrap();
        assert!(encoded.len() < crate::fabric::MAX_FABRIC_CHECKPOINT_BYTES);
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

use tokio::time::timeout;

async fn endpoint() -> (Endpoint, MemoryLookup) {
    crate::loopback_endpoint(None).await
}

/// Reads personal fabric frames on every connection it accepts, as the
/// actor does on the connection of each session with one of the person's
/// devices.
#[derive(Debug, Clone)]
struct FabricReader(mpsc::Sender<InternalEvent>);

impl ProtocolHandler for FabricReader {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        read_personal_fabric(connection, self.0.clone(), MembershipEpoch::for_test(1));
        Ok(())
    }
}

/// One of a person's devices as a test runs it ([`crate::siblings`]),
/// joined to one topic, its events and its siblings' heard.
pub(super) struct Device {
    pub(super) cmd: mpsc::Sender<NetCmd>,
    pub(super) rx: mpsc::Receiver<NetEvent>,
    pub(super) task: tokio::task::JoinHandle<()>,
    pub(super) id: EndpointId,
    pub(super) addr: EndpointAddr,
    pub(super) endpoint: Endpoint,
    pub(super) lookup: MemoryLookup,
    /// The person's root key.
    pub(super) root: [u8; 32],
    _state: tempfile::TempDir,
}

/// The device `key` of the person `root`, joined to topic `ws`, hearing the
/// person's other devices, retrying what is missing every `tick`.
pub(super) async fn device_of(
    root: &ed25519_dalek::SigningKey,
    key: ed25519_dalek::SigningKey,
    ws: WorkspaceId,
    tick: Duration,
) -> Device {
    let state = tempfile::tempdir().unwrap();
    let sibling = crate::siblings::spawn_ticking(root, key, state.path(), tick).await;
    let (on_event, rx) = mpsc::channel(256);
    sibling
        .cmd
        .send(NetCmd::JoinWorkspace {
            ws,
            on_event: on_event.clone(),
        })
        .await
        .unwrap();
    sibling
        .cmd
        .send(NetCmd::WatchSiblings { on_event })
        .await
        .unwrap();
    Device {
        cmd: sibling.cmd,
        rx,
        task: sibling.task,
        id: sibling.addr.id,
        addr: sibling.addr,
        endpoint: sibling.endpoint,
        lookup: sibling.lookup,
        root: root.verifying_key().to_bytes(),
        _state: state,
    }
}

/// `count` devices of one person on topic `ws`, each told where the others
/// are, once each has a session with every other.
pub(super) async fn linked(ws: WorkspaceId, count: usize) -> Vec<Device> {
    let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
    let mut devices = Vec::new();
    for _ in 0..count {
        let key = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        devices.push(device_of(&root, key, ws, HEARTBEAT_INTERVAL).await);
    }
    link(&devices.iter().collect::<Vec<_>>()).await;
    let ids: Vec<_> = devices.iter().map(|device| device.id).collect();
    for device in &mut devices {
        let mut waiting: HashSet<_> = ids.iter().copied().filter(|id| *id != device.id).collect();
        tilo_test_env::within("the devices link", async {
            while !waiting.is_empty() {
                if let Some(NetEvent::Sibling { peer, .. }) = device.rx.recv().await {
                    waiting.remove(&peer);
                }
            }
        })
        .await;
    }
    devices
}

/// Two linked devices, by name.
pub(super) fn two(devices: Vec<Device>) -> [Device; 2] {
    let Ok(two) = <[Device; 2]>::try_from(devices) else {
        panic!("two devices were linked")
    };
    two
}

/// Three linked devices, by name.
pub(super) fn three(devices: Vec<Device>) -> [Device; 3] {
    let Ok(three) = <[Device; 3]>::try_from(devices) else {
        panic!("three devices were linked")
    };
    three
}

/// Tell each of `devices` where each other one is.
pub(super) async fn link(devices: &[&Device]) {
    for device in devices {
        for other in devices.iter().filter(|other| other.id != device.id) {
            device
                .cmd
                .send(NetCmd::Connect {
                    addr: other.addr.clone(),
                })
                .await
                .unwrap();
        }
    }
}

pub(super) async fn wait_for_event(
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

/// Two of a person's devices keep one connection between them: their
/// session and every fabric frame and transfer run on it, both ways, as an
/// idle connection costs both devices its keep-alives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_devices_keep_one_connection_for_their_session_both_ways() {
    let [mut a, mut b] = two(linked(45, 2).await);
    let (a_id, b_id, root) = (a.id, b.id, a.root);
    let update = |author: EndpointId| NeighbourMsg::PersonalWorkspaces {
        user_public_key: root,
        update: Box::new(tilo_user_identity::DirectoryUpdate {
            author_device_public_key: *author.as_bytes(),
            authored_at_ns: 1,
            delta: vec![1, 2, 3],
            signature: [0; 64],
        }),
    };
    for (cmd, rx, from) in [(&a.cmd, &mut b.rx, a_id), (&b.cmd, &mut a.rx, b_id)] {
        cmd.send(NetCmd::PublishToSiblings { msg: update(from) })
            .await
            .unwrap();
        wait_for_event("the other device's workspace list", rx, |event| {
            matches!(
                event,
                NetEvent::SiblingMsg {
                    peer,
                    msg: NeighbourMsg::PersonalWorkspaces { .. },
                    ..
                } if *peer == from
            )
        })
        .await;
        cmd.send(NetCmd::PublishPersonalFabric {
            user_public_key: root,
            payload: PersonalFabricPayload::Request,
        })
        .await
        .unwrap();
        wait_for_event("the other device's fabric request", rx, |event| {
            matches!(
                event,
                NetEvent::PersonalFabric {
                    peer,
                    payload: PersonalFabricPayload::Request,
                    ..
                } if *peer == from
            )
        })
        .await;
    }
    for (name, endpoint) in [("A", &a.endpoint), ("B", &b.endpoint)] {
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

/// A device proves who it is with a certificate of the person's root for
/// its own endpoint; a device of another person is no sibling.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_of_another_person_is_no_sibling() {
    let key = || ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
    let (mine, theirs) = (key(), key());
    let mut a = device_of(&mine, key(), 48, HEARTBEAT_INTERVAL).await;
    let b = device_of(&theirs, key(), 48, HEARTBEAT_INTERVAL).await;
    link(&[&a, &b]).await;
    while let Ok(Some(event)) = timeout(Duration::from_secs(1), a.rx.recv()).await {
        assert!(
            !matches!(event, NetEvent::Sibling { .. }),
            "another person's device became a sibling"
        );
    }
}

/// A device on a runtime of its own, on a thread of its own, that a test
/// freezes whole, as a laptop with its lid closed: its endpoint, its
/// connections' timers and its actor all stand still, and on waking each
/// finds its time has passed.
struct Sleeper {
    runtime: tokio::runtime::Handle,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Sleeper {
    fn new() -> Self {
        let (runtime_tx, runtime) = std::sync::mpsc::channel();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime_tx.send(runtime.handle().clone()).unwrap();
            runtime.block_on(async {
                let _ = stopped.await;
            });
        });
        Self {
            runtime: runtime.recv().unwrap(),
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    /// Stand everything on this runtime still until the returned waker is
    /// dropped.
    fn freeze(&self) -> std::sync::mpsc::Sender<()> {
        let (waker, woken) = std::sync::mpsc::channel::<()>();
        self.runtime.spawn(async move {
            let _ = woken.recv();
        });
        waker
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        let _ = self.stop.take().map(|stop| stop.send(()));
        let _ = self.thread.take().map(std::thread::JoinHandle::join);
    }
}

/// A device asleep for longer than its connection's idle timeout loses its
/// session, and its sibling drops it. On waking the two are linked again
/// soon, each side hears the other join, and what the device asks for on
/// that join reaches its sibling.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_asleep_past_the_idle_timeout_is_linked_again_on_waking() {
    let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
    let key = || ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
    let mut a = device_of(&root, key(), 47, HEARTBEAT_INTERVAL).await;
    let sleeper = Sleeper::new();
    let b_root = root.clone();
    let b_key = key();
    let mut b = sleeper
        .runtime
        .spawn(async move { device_of(&b_root, b_key, 47, HEARTBEAT_INTERVAL).await })
        .await
        .unwrap();
    let (a_id, b_id) = (a.id, b.id);
    link(&[&a, &b]).await;
    let joined = |peer: EndpointId| move |event: &NetEvent| matches!(event, NetEvent::Sibling { peer: joined, .. } if *joined == peer);
    wait_for_event("A hears B join", &mut a.rx, joined(b_id)).await;
    wait_for_event("B hears A join", &mut b.rx, joined(a_id)).await;

    let waker = sleeper.freeze();
    wait_for_event(
        "A drops the sleeping device",
        &mut a.rx,
        |event| matches!(event, NetEvent::SiblingLeft { peer, .. } if *peer == b_id),
    )
    .await;
    let woke = Instant::now();
    drop(waker);

    wait_for_event("A hears B join again", &mut a.rx, joined(b_id)).await;
    wait_for_event("B hears A join again", &mut b.rx, joined(a_id)).await;
    b.cmd
        .send(NetCmd::PublishPersonalFabric {
            user_public_key: b.root,
            payload: PersonalFabricPayload::Request,
        })
        .await
        .unwrap();
    wait_for_event("B's request after waking", &mut a.rx, |event| {
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
    assert!(
        woke.elapsed() < PEER_TIMEOUT,
        "linked again {:?} after waking, as long as a dead link lasts",
        woke.elapsed()
    );
}

/// A device that restarts on the same key hears its sibling's fabric at
/// once: the sibling answers on a link to the restarted device, not on the
/// one its old run left behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restarted_device_hears_its_siblings_fabric_at_once() {
    let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
    let b_key = ed25519_dalek::SigningKey::from_bytes(&[0x5B; 32]);
    let a_key = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
    let mut pair = vec![
        device_of(&root, a_key, 46, HEARTBEAT_INTERVAL).await,
        device_of(&root, b_key.clone(), 46, HEARTBEAT_INTERVAL).await,
    ];
    link(&[&pair[0], &pair[1]]).await;
    let (a_id, b_id) = (pair[0].id, pair[1].id);
    wait_for_event(
        "the pair links",
        &mut pair[0].rx,
        |event| matches!(event, NetEvent::Sibling { peer, .. } if *peer == b_id),
    )
    .await;
    // B's first run ends: its actor stops and its endpoint closes.
    let b = pair.pop().unwrap();
    drop(b.cmd);
    b.task.await.unwrap();
    let mut a = pair.pop().unwrap();

    let mut restarted = device_of(&root, b_key, 46, HEARTBEAT_INTERVAL).await;
    link(&[&a, &restarted]).await;
    wait_for_event(
        "the restarted device links",
        &mut restarted.rx,
        |event| matches!(event, NetEvent::Sibling { peer, .. } if *peer == a_id),
    )
    .await;

    let started = Instant::now();
    restarted
        .cmd
        .send(NetCmd::PublishPersonalFabric {
            user_public_key: a.root,
            payload: PersonalFabricPayload::Request,
        })
        .await
        .unwrap();
    wait_for_event("the restarted device's request", &mut a.rx, |event| {
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
    a.cmd
        .send(NetCmd::PublishPersonalFabricTarget {
            user_public_key: a.root,
            target: b_id,
            payload: PersonalFabricPayload::Action(vec![6, 6]),
        })
        .await
        .unwrap();
    wait_for_event("the sibling's answer", &mut restarted.rx, |event| {
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
        user_public_key: [0xA5; 32],
        payload: PersonalFabricPayload::Snapshot(vec![1, 2, 3]),
    };
    let valid = postcard::to_allocvec(&frame).unwrap();
    assert_eq!(valid[32], 1, "the legacy Snapshot payload tag stays fixed");
    let mut future = valid.clone();
    future[32] = 9; // First unsupported tag after optional connector observations.
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
        payload: PersonalFabricPayload::Snapshot(bytes), ..
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
        .send(InternalEvent::IdentityMaterialChanged)
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
        Ok(InternalEvent::IdentityMaterialChanged)
    ));
}

/// Two of a person's devices hear who the other is as soon as they link,
/// from the certificate it proves on their session, and stream a window
/// directly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_devices_exchange_and_announce_identity_over_iroh() {
    let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
    let mut pair = Vec::new();
    for _ in 0..2 {
        let key = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        pair.push(device_of(&root, key, 42, HEARTBEAT_INTERVAL).await);
    }
    link(&[&pair[0], &pair[1]]).await;
    let ids = [pair[0].id, pair[1].id];
    for (device, other) in pair.iter_mut().zip([ids[1], ids[0]]) {
        wait_for_event(
            "the other device's certificate should not wait for the heartbeat",
            &mut device.rx,
            |event| {
                matches!(
                    event,
                    NetEvent::Sibling { peer, certs, .. }
                        if *peer == other
                            && certs.iter().any(|cert| {
                                cert.device_public_key == *other.as_bytes()
                                    && cert.user_public_key == root.verifying_key().to_bytes()
                            })
                )
            },
        )
        .await;
    }

    let dedicated_before =
        DEDICATED_SURFACE_FRAMES_RECEIVED.load(std::sync::atomic::Ordering::Relaxed);
    pair[0]
        .cmd
        .send(NetCmd::Publish {
            ws: 42,
            msg: NeighbourMsg::SurfaceFrame {
                surface: 7,
                source_peer: crate::behaviour::peer_liveness_handle(&ids[0]),
                encoded_h264: vec![0, 0, 0, 1, 0x65],
                ts: 99,
                seq: 0,
            },
        })
        .await
        .unwrap();
    wait_for_event("A's window frame at B", &mut pair[1].rx, |event| {
        matches!(
            event,
            NetEvent::Msg {
                msg: NeighbourMsg::SurfaceFrame {
                    surface: 7,
                    ts: 99,
                    ..
                },
                ..
            }
        )
    })
    .await;
    tilo_test_env::within(
        "surface frame should arrive through the dedicated QUIC ALPN",
        async {
            while DEDICATED_SURFACE_FRAMES_RECEIVED.load(std::sync::atomic::Ordering::Relaxed)
                <= dedicated_before
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        },
    )
    .await;
}

/// What this actor's peers delivered to it so far, by kind.
async fn tally_of(cmd: &mpsc::Sender<NetCmd>) -> std::collections::BTreeMap<&'static str, u64> {
    let (reply, answer) = tokio::sync::oneshot::channel();
    cmd.send(NetCmd::WireTally(reply)).await.unwrap();
    answer.await.unwrap().snapshot()
}

/// Wait on `rx` until the other device has said who it is and what it can
/// decode, as it does once when their session comes up.
async fn await_identity_and_codecs(rx: &mut mpsc::Receiver<NetEvent>) {
    let (mut identity, mut codecs) = (false, false);
    while !(identity && codecs) {
        match tilo_test_env::within("the other device's identity and codecs", rx.recv())
            .await
            .unwrap()
        {
            NetEvent::Sibling { .. } => identity = true,
            NetEvent::SiblingMsg {
                msg: NeighbourMsg::MediaCapabilities { .. },
                ..
            } => codecs = true,
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn linked_devices_keep_liveness_on_the_transport_and_say_nothing_while_idle() {
    // The upkeep tick runs every 50 ms here, a hundred times its pace in
    // a session, so a second of idling is a hundred ticks.
    const TICK: Duration = Duration::from_millis(50);
    let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
    let key = || ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
    let mut a = device_of(&root, key(), 42, TICK).await;
    let mut b = device_of(&root, key(), 42, TICK).await;
    link(&[&a, &b]).await;

    // Phase `a_new_device_hears_identity_and_codecs_once_per_session`.
    await_identity_and_codecs(&mut a.rx).await;
    await_identity_and_codecs(&mut b.rx).await;

    // Phase `a_linked_idle_pair_sends_no_application_message`. Before
    // the transport carried liveness every tick sent a heartbeat, an
    // identity hello and a codec advert per topic.
    tokio::time::sleep(TICK * 4).await;
    let (before_a, before_b) = (tally_of(&a.cmd).await, tally_of(&b.cmd).await);
    tokio::time::sleep(TICK * 20).await;
    assert_eq!(
        tally_of(&a.cmd).await,
        before_a,
        "a heard nothing while idle"
    );
    assert_eq!(
        tally_of(&b.cmd).await,
        before_b,
        "b heard nothing while idle"
    );
    assert!(
        std::iter::from_fn(|| a.rx.try_recv().ok())
            .chain(std::iter::from_fn(|| b.rx.try_recv().ok()))
            .all(|event| !matches!(event, NetEvent::SiblingLeft { .. })),
        "a silent, linked device stays a sibling"
    );

    // Phase `a_departed_device_is_dropped_when_its_session_ends`. A device
    // that shuts down closes its session; it is dropped then, not after a
    // timeout.
    drop(b.cmd);
    b.task.await.unwrap();
    let gone = Instant::now();
    tilo_test_env::within("the departed device to be dropped", async {
        loop {
            if let Some(NetEvent::SiblingLeft { .. }) = a.rx.recv().await {
                return;
            }
        }
    })
    .await;
    let took = gone.elapsed();
    assert!(
        took <= Duration::from_secs(3),
        "dropped {took:?} after it left"
    );
    drop(b.rx);
    drop(a.cmd);
    a.task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn direct_collaboration_traffic_reaches_only_its_exact_target() {
    // Phase `dead_surface_viewer_does_not_stall_later_collaboration_messages`.
    {
        let [a, b, mut c] = three(linked(44, 3).await);
        let (cmd_a, rx_c, c_id, root) = (&a.cmd, &mut c.rx, c.id, a.root);
        let source_peer = crate::behaviour::peer_liveness_handle(&a.id);

        // Leave A's authenticated membership evidence intact while making B's
        // dedicated media address unreachable. A direct dial may consume the
        // full media timeout, but it must happen outside the actor loop.
        b.task.abort();
        let _ = b.task.await;
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
            let probe = vec![0x7e, u8::try_from(attempt).unwrap()];
            cmd_a
                .send(NetCmd::PublishPersonalFabricTarget {
                    user_public_key: root,
                    target: c_id,
                    payload: PersonalFabricPayload::Action(probe.clone()),
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
                    NetEvent::PersonalFabric {
                        payload: PersonalFabricPayload::Action(ref bytes),
                        ..
                    } if *bytes == probe
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
        a.task.abort();
        c.task.abort();
    }
    // Phase `personal_fabric_action_reaches_only_the_exact_target`.
    {
        let [a, mut b, mut c] = three(linked(44, 3).await);
        let (cmd_a, b_id) = (&a.cmd, b.id);
        let (rx_b, rx_c) = (&mut b.rx, &mut c.rx);

        cmd_a
            .send(NetCmd::PublishPersonalFabricTarget {
                user_public_key: a.root,
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
        let [a, mut b, mut c] = three(linked(43, 3).await);
        let (cmd_a, a_id, b_id) = (&a.cmd, a.id, b.id);
        let (rx_b, rx_c) = (&mut b.rx, &mut c.rx);

        for _ in 0..2 {
            cmd_a
                .send(NetCmd::PublishWorkspaceControl {
                    ws: 43,
                    msg: NeighbourMsg::RemoteInput {
                        peer: crate::behaviour::peer_liveness_handle(&a_id),
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
    // Phase `personal_fabric_reaches_the_persons_device`.
    {
        let [a, mut b] = two(linked(43, 2).await);
        let (cmd_a, rx_b, root) = (&a.cmd, &mut b.rx, a.root);

        cmd_a
            .send(NetCmd::PublishPersonalFabric {
                user_public_key: root,
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
                    user_public_key,
                    payload: PersonalFabricPayload::Snapshot(bytes),
                    ..
                } if user_public_key == root && bytes == vec![1, 2, 3]
            ) {
                break;
            }
        }
    }
    // Phase `personal_transfer_streams_in_order_with_a_verified_finish_hash`.
    {
        // Progress is best-effort; each device's events leave room for every
        // chunk's report while the receiver side is drained first.
        let [mut a, mut b] = two(linked(45, 2).await);
        let (cmd_a, a_id, b_id, root) = (&a.cmd, a.id, b.id, a.root);
        let (rx_a, rx_b) = (&mut a.rx, &mut b.rx);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private-host-name.flac");
        let bytes = vec![0x9a; PERSONAL_TRANSFER_CHUNK_BYTES.saturating_add(31)];
        std::fs::write(&path, &bytes).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let transfer_id = [0x45; 16];
        let request_id = [0x46; 16];
        cmd_a
            .send(NetCmd::PublishNearbyTransfer {
                user_public_key: root,
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
                user_public_key: root,
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

#[test]
fn a_held_streams_start_survives_its_frames_being_superseded() {
    let frame = |ts, encoded_h264| NeighbourMsg::SurfaceFrame {
        surface: 31,
        source_peer: 1,
        encoded_h264,
        ts,
        seq: ts,
    };
    let start = NeighbourMsg::SurfaceStreamStart {
        surface: 31,
        source_peer: 1,
        width: 64,
        height: 64,
        codec: "h264".into(),
        ts: 0,
        sender_monotonic_ns_at_send: 0,
        colorimetry: None,
        hdr_static_metadata: None,
        bit_depth: 0,
        app_id: String::new(),
        tab: 0,
    };
    let mut backlog = SurfaceFrameBacklog::default();
    backlog.lose_references();
    assert!(
        backlog.admit(&start),
        "a stream's start waits for no keyframe"
    );
    backlog.push(start);
    backlog.push(frame(1, vec![0, 0, 1, 0x65]));
    backlog.push(frame(2, vec![0, 0, 1, 0x41]));
    // A newer keyframe supersedes the older chain, not the start.
    backlog.push(frame(3, vec![0, 0, 1, 0x65]));
    assert!(matches!(
        backlog.pop(),
        Some(NeighbourMsg::SurfaceStreamStart { .. })
    ));
    assert!(matches!(
        backlog.pop(),
        Some(NeighbourMsg::SurfaceFrame { ts: 3, .. })
    ));
    assert!(backlog.pop().is_none());
    assert_eq!(backlog.bytes, 0);
}
