//! End-to-end peer-sync test using two `Actor`s over TCP loopback.
//!
//! Validates:
//!   1. Two actors converge on a workspace append via `SyncResp`.
//!   2. Cursor presence broadcasts (`PresenceMoved`) arrive at the other peer.
//!   3. Invite token round-trips through `make_invite_token` /
//!      `validate_invite_token` (signature gates correctness).
//!
//! Modelled on the in-crate `two_actors_exchange_publish_via_loopback` test
//! in `src/network/actor.rs`, but lives in `tests/` so it exercises the
//! crate's public API surface only and runs as a separate binary in CI.

use std::time::Duration;

use libp2p::futures::StreamExt;
use libp2p::swarm::SwarmEvent;
use libp2p::{Multiaddr, Swarm, noise, tcp, yamux};
use shell::network::{
    Actor, NeighbourMsg, NetCmd, NetEvent, NiriosBehaviour,
    invite::{make_invite_token, validate_invite_token},
};
use shell::shell::state::{PeerOrigin, WorkspaceTree};
use tokio::sync::mpsc;
use tokio::time::timeout;

const WS: u64 = 42;

/// Build a swarm using the same transport stack as the in-crate actor
/// test — TCP + noise + yamux + the project's combined behaviour.
fn build_swarm() -> Swarm<NiriosBehaviour> {
    libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .expect("tcp transport")
        .with_behaviour(|key| NiriosBehaviour::new(key).expect("behaviour"))
        .expect("with_behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build()
}

/// Start `swarm` listening on an ephemeral loopback port, drain its first
/// `NewListenAddr` event, and return the resulting concrete multiaddr.
async fn listen_loopback(swarm: &mut Swarm<NiriosBehaviour>) -> Multiaddr {
    swarm
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .expect("listen_on");
    timeout(Duration::from_secs(5), async {
        loop {
            if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
                return address;
            }
        }
    })
    .await
    .expect("listen addr within 5 s")
}

/// Build, listen, and join a workspace. Returns the cmd channel, the
/// event receiver, and the listening multiaddr.
async fn spawn_actor() -> (mpsc::Sender<NetCmd>, mpsc::Receiver<NetEvent>, Multiaddr) {
    let mut swarm = build_swarm();
    let addr = listen_loopback(&mut swarm).await;
    let (cmd_tx, _join) = Actor::spawn(swarm);
    let (ev_tx, ev_rx) = mpsc::channel::<NetEvent>(64);
    cmd_tx
        .send(NetCmd::JoinWorkspace {
            ws: WS,
            on_event: ev_tx,
        })
        .await
        .expect("join");
    (cmd_tx, ev_rx, addr)
}

/// Wait until `ev_rx` reports any `PeerJoined` for our workspace. The
/// gossipsub mesh is up at that point, so subsequent publishes are
/// guaranteed to be delivered (modulo timing for the very first one,
/// which we still hedge with a brief sleep at the call site).
async fn wait_peer_joined(ev_rx: &mut mpsc::Receiver<NetEvent>) {
    timeout(Duration::from_secs(10), async {
        loop {
            match ev_rx.recv().await {
                Some(NetEvent::PeerJoined { .. }) => return,
                Some(_) => continue,
                None => panic!("event channel closed before PeerJoined"),
            }
        }
    })
    .await
    .expect("PeerJoined within 10 s");
}

/// Two actors over TCP loopback exchange a workspace-append change via
/// `SyncResp` and the receiver's tree converges on the new workspace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_actors_exchange_a_workspace_append_change() {
    let (cmd_a, mut ev_rx_a, addr_a) = spawn_actor().await;
    let (cmd_b, mut ev_rx_b, _addr_b) = spawn_actor().await;

    // Have B dial A so the gossipsub mesh forms.
    cmd_b
        .send(NetCmd::Connect { addr: addr_a })
        .await
        .expect("dial");

    // Block until B sees A join the topic — that's our mesh-up signal.
    wait_peer_joined(&mut ev_rx_b).await;
    // Brief settle so the first publish is delivered reliably.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // A constructs a tree with a workspace, which mints that workspace's
    // room, and exports that room's document. Replication is per room:
    // what crosses the wire is one room's own state, so what A ships is
    // addressed by A's room id rather than by A's local workspace handle
    // (which means nothing on B).
    let mut tree_a = WorkspaceTree::default();
    let local_a = tree_a.append_empty_workspace("Renamed");
    let room = tree_a.room_of(local_a).expect("a fresh workspace has a room");
    let changes = tree_a
        .export_room_changes_since(room, &[])
        .expect("A is in the room it is exporting");
    assert!(!changes.is_empty(), "the room must produce a change blob");

    // A publishes a SyncResp with the change blobs.
    cmd_a
        .send(NetCmd::Publish {
            ws: WS,
            msg: NeighbourMsg::SyncResp {
                changes: changes.clone(),
                room: room.to_string(),
            },
        })
        .await
        .expect("publish");

    // B should observe the SyncResp. The reconnect-sync path also emits a
    // roomless `SyncReq` on PeerJoined, so the loop drains until it sees
    // the SyncResp variant we care about.
    let received = timeout(Duration::from_secs(10), async {
        loop {
            match ev_rx_b.recv().await {
                Some(NetEvent::Msg {
                    peer,
                    msg: NeighbourMsg::SyncResp { changes, room },
                    ..
                }) => return Some((changes, room, peer)),
                Some(_) => continue,
                None => return None,
            }
        }
    })
    .await
    .expect("SyncResp within 10 s")
    .expect("event channel still open");
    let (received_changes, received_room, sender) = received;
    assert_eq!(
        received_room,
        room.to_string(),
        "the response must name the room it carries"
    );

    // B is in the same room — its own workspace, its own local id, joined
    // to A's room the way accepting an invite joins it. Apply and assert
    // convergence on the room's state.
    let mut tree_b = WorkspaceTree::default();
    let local_b = tree_b.append_empty_workspace("Placeholder");
    assert!(tree_b.set_room(local_b, room), "B joins A's room");
    let merged = tree_b
        .apply_room_changes(
            room,
            &received_changes,
            &PeerOrigin::peer(&sender),
        )
        .expect("apply");
    assert!(merged, "the round must change B's copy of the room");

    let synced = tree_b
        .workspaces
        .iter()
        .find(|w| w.id == local_b)
        .expect("B keeps its own workspace handle");
    assert_eq!(synced.name, "Renamed");
    assert_eq!(synced.room, room);

    // Drain so the actors shut down cleanly when we exit the test.
    let _ = ev_rx_a.try_recv();
}

/// `PresenceMoved` round-trips between two actors over loopback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn presence_moved_round_trips_between_two_actors() {
    let (cmd_a, _ev_rx_a, addr_a) = spawn_actor().await;
    let (cmd_b, mut ev_rx_b, _addr_b) = spawn_actor().await;
    cmd_b
        .send(NetCmd::Connect { addr: addr_a })
        .await
        .expect("dial");
    wait_peer_joined(&mut ev_rx_b).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let presence = NeighbourMsg::PresenceMoved {
        peer: 7,
        user: "alice".into(),
        key: "local-cursor".into(),
        point: (123.0, 456.0),
        ts: 1_700_000_000,
    };
    cmd_a
        .send(NetCmd::Publish {
            ws: WS,
            msg: presence.clone(),
        })
        .await
        .expect("publish");

    let received = timeout(Duration::from_secs(10), async {
        loop {
            match ev_rx_b.recv().await {
                Some(NetEvent::Msg {
                    msg: m @ NeighbourMsg::PresenceMoved { .. },
                    ..
                }) if m == presence => return Some(m),
                Some(_) => continue,
                None => return None,
            }
        }
    })
    .await
    .expect("PresenceMoved within 10 s")
    .expect("event channel still open");
    assert_eq!(received, presence);
}

/// Sanity check that the public invite-token API is reachable from an
/// integration-test binary and survives a make/validate round-trip.
/// In-crate unit tests already cover tampering / expiry; this is a
/// minimal cross-crate-boundary smoke test.
#[test]
fn invite_token_round_trips() {
    use shell::shell::state::{Workspace, WorkspaceId};

    let kp = libp2p::identity::Keypair::generate_ed25519();
    let workspace = Workspace::test_default(WorkspaceId(99), "Shared");
    let token = make_invite_token(&kp, &workspace, "/ip4/127.0.0.1/tcp/8888", 3600).expect("mint");
    let parsed = validate_invite_token(&token).expect("validate");
    assert_eq!(parsed.workspace_id, 99);
    assert_eq!(parsed.owner_addr, "/ip4/127.0.0.1/tcp/8888");
    // The room the invite grants access to has to survive the crate
    // boundary too — it is the field the receiver adopts, and a token
    // that arrived without it would join a topic and reach nobody.
    assert_eq!(parsed.room_id, workspace.room.to_string());
}

/// End-to-end smoke test — A encodes an RGBA gradient, publishes the
/// NALU bytes as a [`NeighbourMsg::SurfaceFrame`], and B decodes the
/// payload back into an image-shaped frame whose dimensions match the
/// source. This is the cross-actor analogue of
/// `network::encoder::tests::gradient_round_trip_psnr_at_least_35_db`
/// — the unit test proves the codec, this test proves the bytes survive
/// the gossipsub round-trip.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn surface_frame_round_trips_through_gossipsub() {
    use shell::network::{H264Decoder, H264Encoder};

    let (cmd_a, _ev_rx_a, addr_a) = spawn_actor().await;
    let (cmd_b, mut ev_rx_b, _addr_b) = spawn_actor().await;
    cmd_b
        .send(NetCmd::Connect { addr: addr_a })
        .await
        .expect("dial");
    wait_peer_joined(&mut ev_rx_b).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 64×48 gradient — large enough for openh264's 16-px macroblock
    // alignment, small enough that the test runs in <2 s.
    const W: u32 = 64;
    const H: u32 = 48;
    let mut rgba = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            rgba.push(((x * 255) / W) as u8);
            rgba.push(((y * 255) / H) as u8);
            rgba.push(128);
            rgba.push(255);
        }
    }
    let mut enc = H264Encoder::new(W, H, 30).expect("encoder");
    let nalus = enc.encode(&rgba).expect("encode");
    assert!(!nalus.is_empty());

    cmd_a
        .send(NetCmd::Publish {
            ws: WS,
            msg: NeighbourMsg::SurfaceFrame {
                surface: 42,
                encoded_h264: nalus.clone(),
                ts: 1_700_000_000,
            },
        })
        .await
        .expect("publish surface frame");

    let received_bytes = timeout(Duration::from_secs(10), async {
        loop {
            match ev_rx_b.recv().await {
                Some(NetEvent::Msg {
                    msg:
                        NeighbourMsg::SurfaceFrame {
                            surface,
                            encoded_h264,
                            ..
                        },
                    ..
                }) if surface == 42 => return Some(encoded_h264),
                Some(_) => continue,
                None => return None,
            }
        }
    })
    .await
    .expect("SurfaceFrame within 10 s")
    .expect("event channel still open");

    assert_eq!(received_bytes, nalus, "gossipsub-relayed bytes must match");

    // Decode confirms the bitstream still represents the original size —
    // i.e. the gossipsub layer didn't corrupt the payload.
    let mut dec = H264Decoder::new();
    let frame = dec.decode(&received_bytes).expect("decode");
    assert_eq!(frame.width, W);
    assert_eq!(frame.height, H);
}

/// Remote-input smoke test — A publishes a `RemoteInput` with a
/// serialised `InputEvent::PointerMotion` payload; B receives it and
/// recovers the same event after deserialisation. End-to-end proof that
/// the wire-level vocabulary survives the gossipsub round-trip.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_input_round_trips_through_gossipsub() {
    use shell::network::InputEvent;

    let (cmd_a, _ev_rx_a, addr_a) = spawn_actor().await;
    let (cmd_b, mut ev_rx_b, _addr_b) = spawn_actor().await;
    cmd_b
        .send(NetCmd::Connect { addr: addr_a })
        .await
        .expect("dial");
    wait_peer_joined(&mut ev_rx_b).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let original = InputEvent::PointerMotion { x: 123.5, y: 67.25 };
    let event_bytes = serde_json::to_vec(&original).expect("serialize event");
    cmd_a
        .send(NetCmd::Publish {
            ws: WS,
            msg: NeighbourMsg::RemoteInput {
                peer: 3,
                surface: 9,
                event: event_bytes.clone(),
            },
        })
        .await
        .expect("publish remote input");

    let received = timeout(Duration::from_secs(10), async {
        loop {
            match ev_rx_b.recv().await {
                Some(NetEvent::Msg {
                    msg:
                        NeighbourMsg::RemoteInput {
                            peer: 3,
                            surface: 9,
                            event,
                        },
                    ..
                }) => return Some(event),
                Some(_) => continue,
                None => return None,
            }
        }
    })
    .await
    .expect("RemoteInput within 10 s")
    .expect("event channel still open");
    assert_eq!(received, event_bytes);

    let parsed: InputEvent = serde_json::from_slice(&received).expect("deserialize");
    assert_eq!(parsed, original);
}
