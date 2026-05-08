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
use quick_desktop::network::{
    Actor, NeighbourMsg, NetCmd, NetEvent, NiriosBehaviour,
    invite::{make_invite_token, validate_invite_token},
};
use quick_desktop::shell::state::WorkspaceTree;
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
            if let SwarmEvent::NewListenAddr { address, .. } =
                swarm.select_next_some().await
            {
                return address;
            }
        }
    })
    .await
    .expect("listen addr within 5 s")
}

/// Build, listen, and join a workspace. Returns the cmd channel, the
/// event receiver, and the listening multiaddr.
async fn spawn_actor() -> (
    mpsc::Sender<NetCmd>,
    mpsc::Receiver<NetEvent>,
    Multiaddr,
) {
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

    // A constructs a tree, appends a fresh workspace through the
    // doc-mirroring mutator, and exports the change blobs.
    // `append_empty_workspace` is public + already mirrors into the
    // automerge doc (Unit B), so we don't need the private
    // `seed_doc_from_cache` helper to drive a peer-visible change.
    let mut tree_a = WorkspaceTree::default();
    let target_id = tree_a.append_empty_workspace("Renamed");
    let changes = tree_a.export_changes_since(&[]);
    assert!(!changes.is_empty(), "append must produce a change blob");

    // A publishes a SyncResp with the change blobs.
    cmd_a
        .send(NetCmd::Publish {
            ws: WS,
            msg: NeighbourMsg::SyncResp {
                changes: changes.clone(),
            },
        })
        .await
        .expect("publish");

    // B should observe the SyncResp. Phase 6's reconnect-sync path also
    // emits an empty `SyncReq { since: vec![] }` on PeerJoined, so the
    // loop drains until it sees the SyncResp variant we care about.
    let received = timeout(Duration::from_secs(10), async {
        loop {
            match ev_rx_b.recv().await {
                Some(NetEvent::Msg {
                    msg: NeighbourMsg::SyncResp { changes },
                    ..
                }) => return Some(changes),
                Some(_) => continue,
                None => return None,
            }
        }
    })
    .await
    .expect("SyncResp within 10 s")
    .expect("event channel still open");

    // Apply the changes to a fresh tree B and assert convergence.
    let mut tree_b = WorkspaceTree::default();
    tree_b.apply_changes(&received).expect("apply");

    let appended = tree_b
        .workspaces
        .iter()
        .find(|w| w.id == target_id)
        .expect("workspace present after sync");
    assert_eq!(appended.name, "Renamed");

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
    let kp = libp2p::identity::Keypair::generate_ed25519();
    let token = make_invite_token(&kp, 99, "/ip4/127.0.0.1/tcp/8888", 3600).expect("mint");
    let parsed = validate_invite_token(&token).expect("validate");
    assert_eq!(parsed.workspace_id, 99);
    assert_eq!(parsed.owner_addr, "/ip4/127.0.0.1/tcp/8888");
}
