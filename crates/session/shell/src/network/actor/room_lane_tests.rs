use super::room_lane::{Direct, Lanes};
use super::*;
use iroh::endpoint::{Connection, presets};
use std::sync::atomic::AtomicUsize;

async fn endpoint() -> (Endpoint, MemoryLookup) {
    let lookup = MemoryLookup::new();
    let endpoint = Endpoint::builder(presets::N0)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .unwrap();
    (endpoint, lookup)
}

/// More than a stream may have in flight before its reader takes some: a
/// write this big waits on a device that never reads.
const STALLING_BYTES: usize = 4 << 20;

/// A device that takes every connection and stream and never reads one:
/// counts the streams opened to it.
#[derive(Debug, Clone, Default)]
struct NeverReads(Arc<AtomicUsize>);

impl ProtocolHandler for NeverReads {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let mut held = Vec::new();
        while let Ok(stream) = connection.accept_uni().await {
            held.push(stream);
            self.0.store(held.len(), Ordering::SeqCst);
        }
        Ok(())
    }
}

/// A device that reads every stream opened to it, and hands on what it read.
#[derive(Debug, Clone)]
struct Reads(mpsc::Sender<Vec<u8>>);

impl ProtocolHandler for Reads {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        while let Ok(mut stream) = connection.accept_uni().await {
            if let Ok(bytes) = stream.read_to_end(STALLING_BYTES).await {
                let _ = self.0.send(bytes).await;
            }
        }
        Ok(())
    }
}

fn direct(addr: EndpointAddr, bytes: Vec<u8>) -> Direct {
    Direct {
        addr,
        cached: None,
        msg: NeighbourMsg::RemoteAction { frame: Vec::new() },
        bytes,
        // Longer than the test: a write to a device that never reads stays
        // stalled throughout.
        budget: Duration::from_secs(3600),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_send_in_one_room_does_not_delay_another_rooms_send() {
    let (sender, _) = endpoint().await;
    let (stalled, _) = endpoint().await;
    let (healthy, _) = endpoint().await;
    let stalled_router = Router::builder(stalled.clone())
        .accept(SURFACE_STREAM_ALPN, NeverReads::default())
        .spawn();
    let (read_tx, mut read) = mpsc::channel(4);
    let healthy_router = Router::builder(healthy.clone())
        .accept(SURFACE_STREAM_ALPN, Reads(read_tx))
        .spawn();
    let gossip = Gossip::builder().spawn(sender.clone());
    let (internal_tx, mut internal) = mpsc::channel(16);
    let mut lanes = Lanes::new(gossip, sender.clone(), internal_tx);

    for _ in 0..3 {
        assert!(
            lanes
                .queue_direct(1, direct(stalled.addr(), vec![0; STALLING_BYTES]))
                .is_ok()
        );
    }
    assert!(
        lanes
            .queue_direct(2, direct(healthy.addr(), b"room two".to_vec()))
            .is_ok()
    );

    let event = tilo_test_env::within("room two's send", internal.recv())
        .await
        .expect("the lanes report");
    let InternalEvent::DirectSent { ws, outcome } = event else {
        panic!("only direct sends were queued");
    };
    assert_eq!(ws, 2, "room one's stalled sends came back first");
    assert!(outcome.delivered);
    assert_eq!(
        tilo_test_env::within("room two's message", read.recv()).await,
        Some(b"room two".to_vec())
    );
    assert!(
        internal.try_recv().is_err(),
        "room one's sends are still waiting on their device"
    );

    drop(lanes);
    stalled_router.shutdown().await.unwrap();
    healthy_router.shutdown().await.unwrap();
    sender.close().await;
}

async fn join_personal(cmd: &mpsc::Sender<NetCmd>, ws: WorkspaceId) -> mpsc::Receiver<NetEvent> {
    let (on_event, events) = mpsc::channel(64);
    cmd.send(NetCmd::JoinPersonalTopic { ws, on_event })
        .await
        .unwrap();
    events
}

/// Room 7 holds only `stalled` besides this device; send it `sends` remote
/// actions too big for a device that never reads.
async fn stall_room_7(
    cmd: &mpsc::Sender<NetCmd>,
    stalled: EndpointId,
    sends: usize,
) -> mpsc::Receiver<NetEvent> {
    let (on_event, events) = mpsc::channel(64);
    cmd.send(NetCmd::SetRoomRoster {
        ws: 7,
        devices: vec![(stalled, None)],
    })
    .await
    .unwrap();
    cmd.send(NetCmd::JoinWorkspace { ws: 7, on_event })
        .await
        .unwrap();
    for _ in 0..sends {
        cmd.send(NetCmd::PublishRemoteAction {
            ws: 7,
            target: stalled,
            msg: NeighbourMsg::RemoteAction {
                frame: vec![0; STALLING_BYTES],
            },
        })
        .await
        .unwrap();
    }
    events
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_room_whose_device_stalls_holds_up_no_other_room() {
    const STALLED_SENDS: usize = 8;
    let (a, lookup_a) = endpoint().await;
    let (b, lookup_b) = endpoint().await;
    let (stalled, _) = endpoint().await;
    let opened = NeverReads::default();
    let stalled_router = Router::builder(stalled.clone())
        .accept(SURFACE_STREAM_ALPN, opened.clone())
        .spawn();
    lookup_a.add_endpoint_info(stalled.addr());
    let b_addr = b.addr();
    let (cmd_a, _a_task) = Actor::spawn(a, lookup_a);
    let (cmd_b, _b_task) = Actor::spawn(b, lookup_b);

    // Room 42 is shared by both desktops.
    let mut rx_a = join_personal(&cmd_a, 42).await;
    let mut rx_b = join_personal(&cmd_b, 42).await;
    cmd_a.send(NetCmd::Connect { addr: b_addr }).await.unwrap();
    loop {
        let event = tilo_test_env::within("the desktops link", rx_a.recv())
            .await
            .unwrap();
        if matches!(event, NetEvent::PeerJoined { .. }) {
            break;
        }
    }

    // In room 7, the one other device takes what is sent to it and never
    // reads it: each remote action waits out its whole budget.
    let _rx_7 = stall_room_7(&cmd_a, stalled.id(), STALLED_SENDS).await;

    // Room 42's publish and presence still go through, and the actor still
    // answers, while room 7's sends wait.
    let (reply, tally) = tokio::sync::oneshot::channel();
    cmd_a.send(NetCmd::WireTally(reply)).await.unwrap();
    tilo_test_env::within("the actor answers", tally)
        .await
        .expect("the actor is running");
    cmd_a
        .send(NetCmd::Publish {
            ws: 42,
            msg: NeighbourMsg::SyncReq {
                since: vec![],
                room: String::new(),
                invite_id: None,
                nonce: 1,
            },
        })
        .await
        .unwrap();
    cmd_a
        .send(NetCmd::Publish {
            ws: 42,
            msg: NeighbourMsg::PresenceMoved {
                peer: 1,
                user: "a".into(),
                key: "cursor".into(),
                point: (1.0, 2.0),
                ts: 1,
                colour: None,
            },
        })
        .await
        .unwrap();
    let (mut published, mut presence) = (false, false);
    while !(published && presence) {
        match tilo_test_env::within("room 42's messages", rx_b.recv())
            .await
            .unwrap()
        {
            NetEvent::Msg {
                ws: 42,
                msg: NeighbourMsg::SyncReq { nonce: 1, .. },
                ..
            } => published = true,
            NetEvent::Msg {
                ws: 42,
                msg: NeighbourMsg::PresenceMoved { .. },
                ..
            } => presence = true,
            _ => {}
        }
    }
    assert!(
        opened.0.load(Ordering::SeqCst) < STALLED_SENDS,
        "room 42 waited for every one of room 7's stalled sends"
    );

    stalled_router.shutdown().await.unwrap();
}
