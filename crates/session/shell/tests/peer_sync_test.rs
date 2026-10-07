//! End-to-end collaboration tests over two real Iroh endpoints.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, Instant};

use iroh::address_lookup::memory::MemoryLookup;
use iroh::endpoint::presets;
use shell::network::{
    Actor, NeighbourMsg, NetCmd, NetEvent, WorkspaceControlRoute,
    invite::{make_invite_token, validate_invite_token},
};
use shell::shell::state::{PeerOrigin, WorkspaceTree};
use tokio::sync::mpsc;
use tokio::time::timeout;

const WS: u64 = 42;

async fn spawn_actor() -> (
    mpsc::Sender<NetCmd>,
    mpsc::Receiver<NetEvent>,
    iroh::EndpointAddr,
) {
    let lookup = MemoryLookup::new();
    let endpoint = iroh::Endpoint::builder(presets::N0)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .expect("bind Iroh endpoint");
    let addr = endpoint.addr();
    let (cmd_tx, _join) = Actor::spawn(endpoint, lookup);
    let (ev_tx, ev_rx) = mpsc::channel(64);
    cmd_tx
        .send(NetCmd::JoinWorkspace {
            ws: WS,
            on_event: ev_tx,
        })
        .await
        .expect("join workspace");
    (cmd_tx, ev_rx, addr)
}

async fn connect_pair(
    a: &mpsc::Sender<NetCmd>,
    a_addr: iroh::EndpointAddr,
    b: &mpsc::Sender<NetCmd>,
    b_addr: iroh::EndpointAddr,
) {
    a.send(NetCmd::Connect { addr: b_addr }).await.unwrap();
    b.send(NetCmd::Connect { addr: a_addr }).await.unwrap();
}

async fn wait_peer_joined(events: &mut mpsc::Receiver<NetEvent>) {
    tilo_test_env::within("Iroh gossip mesh formed", async {
        while let Some(event) = events.recv().await {
            if matches!(event, NetEvent::PeerJoined { .. }) {
                return;
            }
        }
        panic!("event channel closed before peer joined");
    })
    .await;
}

async fn wait_remote_input(events: &mut mpsc::Receiver<NetEvent>, surface: u64, expected: u8) {
    tilo_test_env::within("exact-target input arrived", async {
        loop {
            if let Some(NetEvent::Msg {
                msg:
                    NeighbourMsg::RemoteInput {
                        surface: received_surface,
                        event,
                        ..
                    },
                ..
            }) = events.recv().await
                && received_surface == surface
                && event.as_slice() == [expected]
            {
                return;
            }
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_actors_exchange_a_workspace_change() {
    let (cmd_a, mut events_a, addr_a) = spawn_actor().await;
    let (cmd_b, mut events_b, addr_b) = spawn_actor().await;
    connect_pair(&cmd_a, addr_a, &cmd_b, addr_b).await;
    wait_peer_joined(&mut events_a).await;
    wait_peer_joined(&mut events_b).await;

    let mut source = WorkspaceTree::default();
    let source_id = source.append_empty_workspace("Renamed");
    let room = source.room_of(source_id).unwrap();
    let changes = source.export_room_changes_since(room, &[]).unwrap();
    cmd_a
        .send(NetCmd::Publish {
            ws: WS,
            msg: NeighbourMsg::SyncResp {
                changes,
                room: room.to_string(),
            },
        })
        .await
        .unwrap();

    let (changes, sender) = tilo_test_env::within("change arrived", async {
        loop {
            if let Some(NetEvent::Msg {
                peer,
                msg: NeighbourMsg::SyncResp { changes, .. },
                ..
            }) = events_b.recv().await
            {
                break (changes, peer);
            }
        }
    })
    .await;

    let mut target = WorkspaceTree::default();
    let target_id = target.append_empty_workspace("Placeholder");
    assert!(target.set_room(target_id, room));
    assert!(
        target
            .apply_room_changes(room, &changes, &PeerOrigin::peer(&sender))
            .unwrap()
    );
    assert_eq!(target.desktop.workspaces[0].name, "Renamed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn presence_round_trips_over_iroh() {
    let (cmd_a, mut events_a, addr_a) = spawn_actor().await;
    let (cmd_b, mut events_b, addr_b) = spawn_actor().await;
    connect_pair(&cmd_a, addr_a, &cmd_b, addr_b).await;
    wait_peer_joined(&mut events_a).await;
    wait_peer_joined(&mut events_b).await;

    let presence = NeighbourMsg::PresenceMoved {
        peer: 7,
        user: "alice".into(),
        key: "cursor".into(),
        point: (123.0, 456.0),
        ts: 1_700_000_000,
        colour: Some("violet".into()),
    };
    cmd_a
        .send(NetCmd::Publish {
            ws: WS,
            msg: presence.clone(),
        })
        .await
        .unwrap();

    tilo_test_env::within("presence arrived", async {
        loop {
            if let Some(NetEvent::Msg { msg, .. }) = events_b.recv().await
                && msg == presence
            {
                break;
            }
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warm_exact_target_input_stays_below_the_interactive_p95_budget() {
    const SURFACE: u64 = 77;
    const SAMPLES: usize = 24;

    let (cmd_a, mut events_a, addr_a) = spawn_actor().await;
    let (cmd_b, mut events_b, addr_b) = spawn_actor().await;
    let target = addr_b.id;
    connect_pair(&cmd_a, addr_a, &cmd_b, addr_b).await;
    wait_peer_joined(&mut events_a).await;
    wait_peer_joined(&mut events_b).await;

    let send = |sequence: u8| NetCmd::PublishWorkspaceControl {
        ws: WS,
        msg: NeighbourMsg::RemoteInput {
            peer: 7,
            surface: SURFACE,
            event: vec![sequence],
        },
        route: WorkspaceControlRoute::Direct { target },
    };
    // The first publication may establish the authenticated QUIC connection;
    // interactive latency is the steady-state path after collaboration has
    // started, so warm it outside the sample distribution.
    cmd_a.send(send(u8::MAX)).await.expect("warm input queued");
    wait_remote_input(&mut events_b, SURFACE, u8::MAX).await;

    let mut latencies = Vec::with_capacity(SAMPLES);
    for sequence in 0..u8::try_from(SAMPLES).expect("sample count fits u8") {
        let started = Instant::now();
        cmd_a.send(send(sequence)).await.expect("input queued");
        wait_remote_input(&mut events_b, SURFACE, sequence).await;
        latencies.push(started.elapsed());
    }
    latencies.sort_unstable();
    let p95 = latencies[latencies.len().saturating_mul(95) / 100];
    eprintln!("warm exact-target Iroh input p95: {p95:?}");
    assert!(
        p95 < Duration::from_millis(50),
        "warm authenticated input p95 must stay below 50 ms, got {p95:?}; samples={latencies:?}"
    );
}

/// A remote-action frame reaches exactly the addressed peer over the direct
/// route, and a byte-identical retry is delivered again rather than being
/// content-deduplicated away: the receiving host deduplicates by invocation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_action_frames_travel_the_exact_target_route() {
    let (cmd_a, mut events_a, addr_a) = spawn_actor().await;
    let (cmd_b, mut events_b, addr_b) = spawn_actor().await;
    let target = addr_b.id;
    connect_pair(&cmd_a, addr_a, &cmd_b, addr_b).await;
    wait_peer_joined(&mut events_a).await;
    wait_peer_joined(&mut events_b).await;

    let frame = b"{\"version\":1}".to_vec();
    for attempt in 0..2 {
        cmd_a
            .send(NetCmd::PublishRemoteAction {
                ws: WS,
                target,
                msg: NeighbourMsg::RemoteAction {
                    frame: frame.clone(),
                },
            })
            .await
            .expect("remote action queued");
        let received = timeout(tilo_test_env::hang_bound(), async {
            loop {
                match events_b.recv().await {
                    Some(NetEvent::Msg {
                        msg: NeighbourMsg::RemoteAction { frame: received },
                        ..
                    }) => return received,
                    Some(_) => {}
                    None => panic!("event channel closed"),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("attempt {attempt} was not delivered"));
        assert_eq!(received, frame);
    }

    // Anything else is refused on this route rather than broadcast.
    cmd_a
        .send(NetCmd::PublishRemoteAction {
            ws: WS,
            target,
            msg: NeighbourMsg::Heartbeat { peer: 1, ts: 1 },
        })
        .await
        .expect("queued");
    let stray = timeout(Duration::from_millis(300), async {
        loop {
            if let Some(NetEvent::Msg {
                msg: NeighbourMsg::Heartbeat { peer: 1, ts: 1 },
                ..
            }) = events_b.recv().await
            {
                return;
            }
        }
    })
    .await;
    assert!(stray.is_err(), "only remote-action frames use this route");
}

#[test]
fn invite_token_carries_an_authenticated_iroh_address() {
    use shell::shell::state::{Workspace, WorkspaceId};

    let key = iroh::SecretKey::generate();
    let addr =
        iroh::EndpointAddr::new(key.public()).with_ip_addr("127.0.0.1:8888".parse().unwrap());
    let workspace = Workspace::new(WorkspaceId(99), "Shared");
    let token = make_invite_token(&key, &workspace, &addr, 3600).unwrap();
    let parsed = validate_invite_token(&token).unwrap();
    assert_eq!(parsed.workspace_id, 99);
    assert_eq!(parsed.owner_addr, addr);
    assert_eq!(parsed.owner_peer_id, key.public().to_string());
}

async fn spawn_same_identity_actor(
    device_seed: u8,
) -> (
    mpsc::Sender<NetCmd>,
    mpsc::Receiver<NetEvent>,
    iroh::EndpointAddr,
) {
    use shell::network::HostIdentity;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
    let root = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
    let key = ed25519_dalek::SigningKey::from_bytes(&[device_seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("Synthetic device {device_seed}"),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    let lookup = MemoryLookup::new();
    let endpoint = iroh::Endpoint::builder(presets::N0)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .address_lookup(lookup.clone())
        .bind()
        .await
        .unwrap();
    let addr = endpoint.addr();
    let identity = HostIdentity {
        device_cert: cert,
        device_signing_key: key,
        workspace_device_certs: HashMap::new(),
        workspace_revocations: HashMap::new(),
        identity_paths: None,
    };
    let (cmd, _join) = Actor::spawn_reporting_addrs(
        endpoint,
        lookup,
        Some(identity),
        Arc::new(Mutex::new(Vec::new())),
    );
    let (events, rx) = mpsc::channel(64);
    cmd.send(NetCmd::JoinWorkspace {
        ws: WS,
        on_event: events,
    })
    .await
    .unwrap();
    (cmd, rx, addr)
}

async fn wait_certified_native_peer(events: &mut mpsc::Receiver<NetEvent>) {
    tilo_test_env::within("same-identity native peer announced", async {
        let mut joined = false;
        let mut certified = false;
        while !(joined && certified) {
            match events.recv().await.expect("certified actor remains live") {
                NetEvent::PeerJoined { .. } => joined = true,
                NetEvent::Msg {
                    peer,
                    msg: NeighbourMsg::IdentityHello { cert, .. },
                    ..
                } => {
                    assert_eq!(
                        cert.device_public_key,
                        *peer.as_bytes(),
                        "certificate binds the transport endpoint"
                    );
                    let identity = shell::network::PeerIdentityStore::new()
                        .insert(PeerOrigin::peer(&peer), *cert, 2)
                        .unwrap();
                    let root = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
                    assert_eq!(identity.user_pubkey, root.verifying_key().to_bytes());
                    certified = true;
                }
                _ => {}
            }
        }
    })
    .await;
}

use shell::network::actor::PersonalFabricPayload;
use shell::shell::device_fabric as native_fabric;
use shell::shell::personal_continuity::DeviceId;

fn native_snapshot(
    device: &DeviceId,
    scale_milli: u16,
    rect: native_fabric::DisplayRect,
) -> native_fabric::DeviceFabricSnapshot {
    let stamp = native_fabric::FabricStamp {
        counter: 1,
        device: device.clone(),
    };
    native_fabric::DeviceFabricSnapshot {
        schema_version: native_fabric::FABRIC_SCHEMA_VERSION,
        owner: device.clone(),
        presence: native_fabric::DevicePresence {
            device: device.clone(),
            label: "Synthetic screen".into(),
            kind: native_fabric::DeviceKind::Laptop,
            capabilities: native_fabric::FabricCapabilities {
                display: true,
                pointer: true,
                keyboard: true,
                ..native_fabric::FabricCapabilities::default()
            },
            online: true,
            last_seen_unix_ms: 1,
            wake_routes: Vec::new(),
            stamp: stamp.clone(),
        },
        display: Some(native_fabric::DisplayPlacement {
            device: device.clone(),
            rect,
            scale_milli,
            stamp,
        }),
        media: Vec::new(),
        clipboard: Vec::new(),
        settings: Vec::new(),
        wifi: Vec::new(),
        speakers: shell::shell::speaker_group::SpeakerFabricState::default(),
        unlock: shell::shell::device_unlock::UnlockFabricState::default(),
    }
}

async fn wait_native_payload(
    events: &mut mpsc::Receiver<NetEvent>,
    expected_peer: iroh::EndpointId,
) -> PersonalFabricPayload {
    tilo_test_env::within("authenticated native fabric payload", async {
        loop {
            match events.recv().await.expect("native actor remains live") {
                NetEvent::PersonalFabric { peer, user_public_key, payload, .. } => {
                    assert_eq!(peer, expected_peer);
                    assert_eq!(user_public_key, ed25519_dalek::SigningKey::from_bytes(&[41; 32]).verifying_key().to_bytes());
                    return payload;
                }
                NetEvent::Msg { msg: NeighbourMsg::SurfaceStreamStart { .. } | NeighbourMsg::SurfaceFrame { .. }, .. } => panic!("native fixture emitted video"),
                _ => {}
            }
        }
    }).await
}

async fn send_native_payload(
    cmd: &mpsc::Sender<NetCmd>,
    target: iroh::EndpointId,
    payload: PersonalFabricPayload,
) {
    cmd.send(NetCmd::PublishPersonalFabricTarget {
        ws: WS,
        user_public_key: ed25519_dalek::SigningKey::from_bytes(&[41; 32])
            .verifying_key()
            .to_bytes(),
        target,
        payload,
    })
    .await
    .unwrap();
}

/// The existing certified personal-fabric lane carries placement and leased
/// input to the native seat. No surface share, stream, enrollment or new router.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_screen_crossing_reaches_the_two_peer_seat_without_video() {
    let (cmd_a, mut events_a, addr_a) = spawn_same_identity_actor(42).await;
    let (cmd_b, mut events_b, addr_b) = spawn_same_identity_actor(43).await;
    let source = DeviceId::new(hex::encode(addr_a.id.as_bytes())).unwrap();
    let target = DeviceId::new(hex::encode(addr_b.id.as_bytes())).unwrap();
    let endpoint_a = addr_a.id;
    let endpoint_b = addr_b.id;
    connect_pair(&cmd_a, addr_a, &cmd_b, addr_b).await;
    wait_certified_native_peer(&mut events_a).await;
    wait_certified_native_peer(&mut events_b).await;
    let mut source_fabric = native_fabric::DeviceFabric::default();
    let mut target_fabric = native_fabric::DeviceFabric::default();
    source_fabric
        .upsert(native_snapshot(
            &source,
            2000,
            native_fabric::DisplayRect {
                x: 0,
                y: 0,
                width: 960,
                height: 540,
            },
        ))
        .unwrap();
    target_fabric
        .upsert(native_snapshot(
            &target,
            1000,
            native_fabric::DisplayRect {
                x: 960,
                y: 0,
                width: 1280,
                height: 720,
            },
        ))
        .unwrap();
    send_native_payload(
        &cmd_b,
        endpoint_a,
        PersonalFabricPayload::Snapshot(target_fabric.encode_checkpoint().unwrap()),
    )
    .await;
    let PersonalFabricPayload::Snapshot(bytes) =
        wait_native_payload(&mut events_a, endpoint_b).await
    else {
        panic!("expected native topology snapshot");
    };
    let received = native_fabric::DeviceFabric::decode_checkpoint(&bytes).unwrap();
    assert_eq!(received.snapshots().len(), 1);
    let remote = received.snapshots().get(&target).unwrap().clone();
    source_fabric.upsert(remote.clone()).unwrap();
    let mut advertised_source = native_fabric::DeviceFabric::default();
    advertised_source
        .upsert(source_fabric.snapshots().get(&source).unwrap().clone())
        .unwrap();
    send_native_payload(
        &cmd_a,
        endpoint_b,
        PersonalFabricPayload::Snapshot(advertised_source.encode_checkpoint().unwrap()),
    )
    .await;
    let PersonalFabricPayload::Snapshot(bytes) =
        wait_native_payload(&mut events_b, endpoint_a).await
    else {
        panic!("expected source topology");
    };
    let received = native_fabric::DeviceFabric::decode_checkpoint(&bytes).unwrap();
    assert_eq!(received.snapshots().len(), 1);
    target_fabric
        .upsert(received.snapshots().get(&source).unwrap().clone())
        .unwrap();
    assert_eq!(
        source_fabric, target_fabric,
        "both native replicas converge on the same logical topology"
    );
    let crossing = source_fabric.cross_right_edge(&source, 205).unwrap();
    assert_eq!(
        (&crossing.to, crossing.target_x, crossing.target_y),
        (&target, 0, 205)
    );
    let event = native_fabric::FabricInputEvent::PointerMotion {
        x: crossing.target_x,
        y: crossing.target_y,
    };
    let lease = assert_native_wire_input(
        &cmd_a,
        &mut events_b,
        (endpoint_a, endpoint_b),
        (&source, &target),
        event,
    )
    .await;
    assert_native_lease_and_topology(&source, &target, source_fabric, remote, &lease);
}

async fn assert_native_wire_input(
    cmd: &mpsc::Sender<NetCmd>,
    events: &mut mpsc::Receiver<NetEvent>,
    endpoints: (iroh::EndpointId, iroh::EndpointId),
    devices: (&DeviceId, &DeviceId),
    event: native_fabric::FabricInputEvent,
) -> native_fabric::InputLease {
    let (source_endpoint, target_endpoint) = endpoints;
    let (source, target) = devices;
    let now = tilo_clock::unix_now_ms();
    let lease = native_fabric::InputLease {
        controller: source.clone(),
        target: target.clone(),
        issued_unix_ms: now,
        expires_unix_ms: now.saturating_add(60_000),
        nonce: [9; 16],
        allow_keyboard: true,
        allow_pointer: true,
    };
    assert!(
        !native_fabric::permits_spatial_input(None, source, target, (lease.nonce, event), now),
        "same identity and topology do not grant input"
    );
    let envelope = native_fabric::PersonalFabricActionEnvelope {
        request_id: [1; 16],
        source: source.clone(),
        target: target.clone(),
        issued_unix_ms: now,
        action: native_fabric::PersonalFabricAction::Input {
            lease_nonce: lease.nonce,
            event,
        },
    };
    send_native_payload(
        cmd,
        target_endpoint,
        PersonalFabricPayload::Action(postcard::to_allocvec(&envelope).unwrap()),
    )
    .await;
    let PersonalFabricPayload::Action(bytes) = wait_native_payload(events, source_endpoint).await
    else {
        panic!("expected native input action");
    };
    let received: native_fabric::PersonalFabricActionEnvelope =
        postcard::from_bytes(&bytes).unwrap();
    received.validate_at(source, target, now).unwrap();
    let native_fabric::PersonalFabricAction::Input { lease_nonce, event } = received.action else {
        panic!("expected input");
    };
    assert!(native_fabric::permits_spatial_input(
        Some(&lease),
        source,
        target,
        (lease_nonce, event),
        now
    ));
    assert_native_receiver_policy(source, event);
    lease
}

fn assert_native_receiver_policy(controller: &DeviceId, event: native_fabric::FabricInputEvent) {
    use shell::compositor::state::State;
    use shell::network::ShellView;
    use smithay::reexports::calloop::EventLoop;
    use smithay::reexports::wayland_server::Display;
    let event_loop: EventLoop<'static, State> = EventLoop::try_new().unwrap();
    let display: Display<State> = Display::new().unwrap();
    let mut state = State::new(event_loop.handle(), display.handle());
    assert!(state.apply_personal_fabric_input(controller, event));
    let pointer = state.seat.get_pointer().unwrap().current_location();
    assert_eq!((pointer.x, pointer.y), (0.0, 205.0));
    assert!(state.apply_personal_fabric_input(
        controller,
        native_fabric::FabricInputEvent::Key {
            keycode: 30,
            pressed: true,
            modifiers: 0
        }
    ));
    assert!(
        state
            .seat
            .get_keyboard()
            .unwrap()
            .pressed_keys()
            .iter()
            .any(|key| u32::from(*key) == 38)
    );
    state.release_personal_fabric_input(controller);
    assert!(
        state.seat.get_keyboard().unwrap().pressed_keys().is_empty(),
        "disconnect/revocation cleanup releases held keys"
    );
    // Keysym lookup must convert back to evdev before the common seat injector.
    for pressed in [true, false] {
        assert!(state.apply_remote_desktop_action(
            shell::network::streaming::RemoteAction::KeyboardKeysym {
                keysym: 0x61,
                pressed
            }
        ));
        assert_eq!(
            state
                .seat
                .get_keyboard()
                .unwrap()
                .pressed_keys()
                .iter()
                .any(|key| u32::from(*key) == 38),
            pressed
        );
    }
    state
        .remote_input_firewall
        .publish_view(ShellView::new(Vec::new(), None, false, false).locked(true));
    assert!(!state.apply_personal_fabric_input(controller, event));
    state
        .remote_input_firewall
        .publish_view(ShellView::new(Vec::new(), None, false, false));
    state.remote_input_firewall.engage_kill_switch();
    assert!(!state.apply_personal_fabric_input(controller, event));
}

fn assert_native_lease_and_topology(
    source: &DeviceId,
    target: &DeviceId,
    mut fabric: native_fabric::DeviceFabric,
    mut remote: native_fabric::DeviceFabricSnapshot,
    lease: &native_fabric::InputLease,
) {
    let event = native_fabric::FabricInputEvent::Key {
        keycode: 30,
        pressed: true,
        modifiers: 0,
    };
    let now = lease.issued_unix_ms;
    assert!(!native_fabric::permits_spatial_input(
        Some(lease),
        source,
        target,
        ([8; 16], event),
        now
    ));
    assert!(!native_fabric::permits_spatial_input(
        Some(lease),
        target,
        source,
        (lease.nonce, event),
        now
    ));
    assert!(!native_fabric::permits_spatial_input(
        Some(lease),
        source,
        target,
        (lease.nonce, event),
        lease.expires_unix_ms.saturating_add(1)
    ));
    assert!(
        !native_fabric::permits_spatial_input(None, source, target, (lease.nonce, event), now),
        "revocation discards the retained lease"
    );
    remote.presence.online = false;
    remote.presence.stamp.counter = 2;
    fabric.upsert(remote.clone()).unwrap();
    assert!(fabric.cross_right_edge(source, 205).is_none());
    remote.presence.online = true;
    remote.presence.stamp.counter = 3;
    remote.display.as_mut().unwrap().rect.x = 1100;
    fabric.upsert(remote.clone()).unwrap();
    assert!(
        fabric.cross_right_edge(source, 205).is_none(),
        "rearrangement leaves a real gap"
    );
    remote.display.as_mut().unwrap().rect.x = 960;
    assert!(
        matches!(
            fabric.upsert(remote.clone()),
            Err(native_fabric::FabricError::ConflictingRevision)
        ),
        "concurrent equal revisions cannot silently replace topology"
    );
    remote.presence.stamp.counter = 4;
    fabric.upsert(remote).unwrap();
    assert!(fabric.cross_right_edge(source, 205).is_some());
    assert!(
        !native_fabric::permits_spatial_input(None, source, target, (lease.nonce, event), now),
        "reconnect does not recreate revoked authority"
    );
}

#[test]
fn native_screen_geometry_rejects_offline_sources_and_invalid_bounds() {
    let source = DeviceId::new("source").unwrap();
    let target = DeviceId::new("target").unwrap();
    let mut fabric = native_fabric::DeviceFabric::default();
    let mut local = native_snapshot(
        &source,
        2000,
        native_fabric::DisplayRect {
            x: 0,
            y: 0,
            width: 960,
            height: 540,
        },
    );
    fabric.upsert(local.clone()).unwrap();
    fabric
        .upsert(native_snapshot(
            &target,
            1000,
            native_fabric::DisplayRect {
                x: 960,
                y: 0,
                width: 1280,
                height: 720,
            },
        ))
        .unwrap();
    assert!(fabric.cross_right_edge(&source, 205).is_some());
    local.presence.online = false;
    local.presence.stamp.counter = 2;
    fabric.upsert(local).unwrap();
    assert!(fabric.cross_right_edge(&source, 205).is_none());
    assert!(
        native_fabric::DisplayRect {
            x: i32::MAX,
            y: 0,
            width: 1,
            height: 1
        }
        .logical_placement()
        .is_none()
    );
    assert!(
        native_fabric::DisplayRect {
            x: 0,
            y: 0,
            width: 0,
            height: 1
        }
        .logical_placement()
        .is_none()
    );
}
