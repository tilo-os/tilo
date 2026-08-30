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

use std::time::Duration;

use iroh::address_lookup::memory::MemoryLookup;
use iroh::endpoint::presets;
use shell::network::{
    Actor, NeighbourMsg, NetCmd, NetEvent,
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
    timeout(Duration::from_secs(15), async {
        while let Some(event) = events.recv().await {
            if matches!(event, NetEvent::PeerJoined { .. }) {
                return;
            }
        }
        panic!("event channel closed before peer joined");
    })
    .await
    .expect("Iroh gossip mesh formed");
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

    let (changes, sender) = timeout(Duration::from_secs(15), async {
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
    .await
    .expect("change arrived");

    let mut target = WorkspaceTree::default();
    let target_id = target.append_empty_workspace("Placeholder");
    assert!(target.set_room(target_id, room));
    assert!(
        target
            .apply_room_changes(room, &changes, &PeerOrigin::peer(&sender))
            .unwrap()
    );
    assert_eq!(target.workspaces[0].name, "Renamed");
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
    };
    cmd_a
        .send(NetCmd::Publish {
            ws: WS,
            msg: presence.clone(),
        })
        .await
        .unwrap();

    timeout(Duration::from_secs(15), async {
        loop {
            if let Some(NetEvent::Msg { msg, .. }) = events_b.recv().await
                && msg == presence
            {
                break;
            }
        }
    })
    .await
    .expect("presence arrived");
}

#[test]
fn invite_token_carries_an_authenticated_iroh_address() {
    use shell::shell::state::{Workspace, WorkspaceId};

    let key = iroh::SecretKey::generate();
    let addr =
        iroh::EndpointAddr::new(key.public()).with_ip_addr("127.0.0.1:8888".parse().unwrap());
    let workspace = Workspace::test_default(WorkspaceId(99), "Shared");
    let token = make_invite_token(&key, &workspace, &addr, 3600).unwrap();
    let parsed = validate_invite_token(&token).unwrap();
    assert_eq!(parsed.workspace_id, 99);
    assert_eq!(parsed.owner_addr, addr);
    assert_eq!(parsed.owner_peer_id, key.public().to_string());
}
