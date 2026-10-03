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
