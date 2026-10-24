//! End-to-end collaboration tests over two real Iroh endpoints.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, Instant};

use iroh::address_lookup::memory::MemoryLookup;
use iroh::endpoint::presets;
use shell::network::{
    Actor, NeighbourMsg, NetCmd, NetEvent, WorkspaceControlRoute,
    invite::{make_invite_token, validate_invite_token},
};
use shell::shell::state::{
    AppKind, PeerOrigin, RoomId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
    Workspace, WorkspaceId, WorkspaceTree, compose_id,
};
use tokio::sync::mpsc;
use tokio::time::timeout;

const WS: u64 = 42;

async fn spawn_actor() -> (
    mpsc::Sender<NetCmd>,
    mpsc::Receiver<NetEvent>,
    iroh::EndpointAddr,
) {
    let (cmd_tx, ev_rx, addr, _endpoint) = spawn_actor_with_endpoint().await;
    (cmd_tx, ev_rx, addr)
}

/// [`spawn_actor`], keeping a handle on the actor's endpoint to read its
/// socket counters.
async fn spawn_actor_with_endpoint() -> (
    mpsc::Sender<NetCmd>,
    mpsc::Receiver<NetEvent>,
    iroh::EndpointAddr,
    iroh::Endpoint,
) {
    let lookup = MemoryLookup::new();
    let endpoint = iroh::Endpoint::builder(presets::N0)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .expect("bind Iroh endpoint");
    let addr = endpoint.addr();
    let (cmd_tx, _join) = Actor::spawn(endpoint.clone(), lookup);
    let (ev_tx, ev_rx) = mpsc::channel(64);
    cmd_tx
        .send(NetCmd::JoinWorkspace {
            ws: WS,
            on_event: ev_tx,
        })
        .await
        .expect("join workspace");
    (cmd_tx, ev_rx, addr, endpoint)
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
    let a_id = addr_a.id;
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
    target
        .join_room("Placeholder", room)
        .expect("joins the room");
    // The invite names A's endpoint as the room's governor.
    assert!(target.pin_room_authority(room, PeerOrigin::peer(&a_id)));
    assert_eq!(sender, a_id, "the change arrives from A");
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

/// A pointer move along a drag: step `step` at `x = step`.
fn drag_move(step: u32) -> NeighbourMsg {
    NeighbourMsg::PresenceMoved {
        peer: 7,
        user: "alice".into(),
        key: "cursor".into(),
        point: (f64::from(step), 0.0),
        ts: u64::from(step),
        colour: None,
    }
}

/// An app's presence in the same room.
fn app_presence(payload: collab_types::PeerPresencePayload) -> NeighbourMsg {
    NeighbourMsg::AppPresenceV2 {
        peer: 7,
        app_id: "so.tilo.word-processor".into(),
        cluster_id: "room".into(),
        presence: collab_types::PeerPresence {
            key: b"field".to_vec(),
            payload,
        },
        colour: None,
    }
}

/// Datagrams both endpoints received: on a two-desktop path, every packet
/// either one sent the other, acknowledgements included.
fn datagrams(a: &iroh::Endpoint, b: &iroh::Endpoint) -> u64 {
    a.metrics()
        .socket
        .recv_datagrams
        .get()
        .saturating_add(b.metrics().socket.recv_datagrams.get())
}

/// Send one move on a key of its own and wait for it: the first message may
/// still settle the connection, which a measured drag must not include.
async fn warm_path(cmd: &mpsc::Sender<NetCmd>, events: &mut mpsc::Receiver<NetEvent>) {
    let warm = NeighbourMsg::PresenceMoved {
        peer: 7,
        user: "alice".into(),
        key: "warm".into(),
        point: (0.0, 0.0),
        ts: 0,
        colour: None,
    };
    cmd.send(NetCmd::Publish {
        ws: WS,
        msg: warm.clone(),
    })
    .await
    .unwrap();
    tilo_test_env::within("the warm-up move arrived", async {
        while let Some(event) = events.recv().await {
            if matches!(event, NetEvent::Msg { msg, .. } if msg == warm) {
                return;
            }
        }
        panic!("event channel closed before the warm-up move");
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
}

/// Each drag move that arrives, as its step and when it arrived, until the
/// resting position (`last`) has.
fn collect_drag(
    mut events: mpsc::Receiver<NetEvent>,
    last: usize,
) -> tokio::task::JoinHandle<Vec<(usize, Instant)>> {
    tokio::spawn(tilo_test_env::within(
        "the drag's resting position arrived",
        async move {
            let mut arrivals = Vec::new();
            while let Some(event) = events.recv().await {
                if let NetEvent::Msg {
                    msg: NeighbourMsg::PresenceMoved { key, ts, .. },
                    ..
                } = event
                    && key == "cursor"
                {
                    let step = usize::try_from(ts).unwrap();
                    arrivals.push((step, Instant::now()));
                    if step == last {
                        return arrivals;
                    }
                }
            }
            panic!("event channel closed after {arrivals:?}");
        },
    ))
}

/// A 30 Hz drag published on the direct (gossip) path, as the shell
/// publishes a moving pointer.
struct DragRun {
    /// Each move that arrived: its step and when.
    received: Vec<(usize, Instant)>,
    /// When each step was published.
    published: Vec<Instant>,
    /// Datagrams the two endpoints exchanged meanwhile.
    packets: u64,
}

impl DragRun {
    /// The shell publishes a moving pointer every 33 ms; 120 moves are a 4 s drag.
    const MOVES: u32 = 120;
    const PRODUCER_GAP: Duration = Duration::from_millis(33);

    async fn run() -> Self {
        let (cmd_a, mut events_a, addr_a, endpoint_a) = spawn_actor_with_endpoint().await;
        let (cmd_b, mut events_b, addr_b, endpoint_b) = spawn_actor_with_endpoint().await;
        connect_pair(&cmd_a, addr_a, &cmd_b, addr_b).await;
        wait_peer_joined(&mut events_a).await;
        wait_peer_joined(&mut events_b).await;
        warm_path(&cmd_a, &mut events_b).await;
        tokio::time::sleep(Duration::from_millis(500)).await;

        let before = datagrams(&endpoint_a, &endpoint_b);
        let start = Instant::now();
        let last = usize::try_from(Self::MOVES).unwrap().saturating_sub(1);
        let listener = collect_drag(events_b, last);
        let mut published = Vec::with_capacity(usize::try_from(Self::MOVES).unwrap());
        for step in 0..Self::MOVES {
            let due = start
                .checked_add(Self::PRODUCER_GAP.saturating_mul(step))
                .unwrap();
            tokio::time::sleep_until(due.into()).await;
            published.push(Instant::now());
            cmd_a
                .send(NetCmd::Publish {
                    ws: WS,
                    msg: drag_move(step),
                })
                .await
                .unwrap();
        }
        let received = listener.await.unwrap();
        let elapsed = start.elapsed();
        let packets = datagrams(&endpoint_a, &endpoint_b).saturating_sub(before);
        let packets_per_minute =
            f64::from(u32::try_from(packets).unwrap()) / elapsed.as_secs_f64() * 60.0;
        eprintln!(
            "GOSSIP_DRAG moves={} messages={} packets={packets} seconds={:.2} \
             packets_per_minute={packets_per_minute:.0}",
            Self::MOVES,
            received.len(),
            elapsed.as_secs_f64(),
        );
        Self {
            received,
            published,
            packets,
        }
    }

    /// The most moves pacing lets through over the drag as it was published:
    /// writes of one slot are at least a gap apart, the first at once, then
    /// one per gap, and the resting position after the last.
    fn most_paced(&self) -> usize {
        let gap = tilo_relay_session::coedit::POINTER_GAP;
        let span = match (self.published.first(), self.published.last()) {
            (Some(first), Some(last)) => last.duration_since(*first),
            _ => Duration::ZERO,
        };
        usize::try_from(span.as_millis().checked_div(gap.as_millis()).unwrap())
            .unwrap()
            .saturating_add(2)
    }

    fn latencies(&self) -> Vec<Duration> {
        self.received
            .iter()
            .map(|(step, at)| at.duration_since(self.published[*step]))
            .collect()
    }
}

/// The direct (gossip) path paces a pointer as a relay session does: a
/// 30 Hz drag reaches the other desktop at no more than 20 Hz, and its
/// resting position always arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_drag_reaches_the_other_desktop_paced_with_its_resting_position() {
    let gap = tilo_relay_session::coedit::POINTER_GAP;
    let run = DragRun::run().await;
    assert!(
        run.packets > 0,
        "the endpoints' socket counters saw nothing"
    );
    assert_eq!(
        run.received.last().map(|(step, _)| *step),
        Some(run.published.len().saturating_sub(1)),
        "the resting position must arrive"
    );
    let most = run.most_paced();
    assert!(
        run.received.len() <= most,
        "{} of {} moves crossed the direct path; paced to {gap:?} at most {most} may",
        run.received.len(),
        DragRun::MOVES
    );
}

/// How promptly the paced drag arrives: about one move per gap, and each
/// shown within 100 ms at p99. Wall-clock time depends on the machine.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "measurement; run with --ignored --nocapture"]
async fn a_paced_drag_arrives_promptly() {
    let run = DragRun::run().await;
    let most = run.most_paced();
    assert!(
        run.received.len().saturating_mul(10) >= most.saturating_mul(8),
        "only {} moves arrived; about {most} should",
        run.received.len()
    );
    let latencies = run.latencies();
    let mut sorted = latencies.clone();
    sorted.sort_unstable();
    let p99 = percentile(&sorted, 99);
    report("direct-path pointer move, publish to receipt", latencies);
    assert!(
        p99 <= Duration::from_millis(100),
        "a move must be shown within 100 ms at p99, got {p99:?}"
    );
}

/// On the direct path a caret passes a held pointer move, and an app's
/// departure never overtakes the move published before it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_caret_passes_a_held_pointer_and_a_departure_follows_it() {
    use collab_types::PeerPresencePayload as P;

    let (cmd_a, mut events_a, addr_a) = spawn_actor().await;
    let (cmd_b, mut events_b, addr_b) = spawn_actor().await;
    connect_pair(&cmd_a, addr_a, &cmd_b, addr_b).await;
    wait_peer_joined(&mut events_a).await;
    wait_peer_joined(&mut events_b).await;

    // The first move goes at once; the second, inside the gap, is held.
    let sent = [
        drag_move(1),
        drag_move(2),
        app_presence(P::Caret { pos: 4 }),
        app_presence(P::Left),
    ];
    for msg in sent.clone() {
        cmd_a.send(NetCmd::Publish { ws: WS, msg }).await.unwrap();
    }
    let arrived = tilo_test_env::within("the drag and the departure arrived", async {
        let mut arrived = Vec::new();
        while let Some(event) = events_b.recv().await {
            if let NetEvent::Msg { msg, .. } = event
                && sent.contains(&msg)
            {
                arrived.push(msg);
                if arrived.len() == sent.len() {
                    return arrived;
                }
            }
        }
        panic!("event channel closed after {arrived:?}");
    })
    .await;
    assert_eq!(
        arrived,
        vec![
            drag_move(1),
            app_presence(P::Caret { pos: 4 }),
            drag_move(2),
            app_presence(P::Left),
        ],
        "the caret passes the held move; the departure follows it"
    );
}

/// Warm exact-target input over real Iroh: every input reaches the addressed
/// peer, each before the next is sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warm_exact_target_input_reaches_its_peer_in_order() {
    let latencies = warm_exact_target_input_latencies().await;
    assert_eq!(latencies.len(), 100, "every input arrived");
}

/// The same inputs stay below the interactive p95 budget. Wall-clock time
/// depends on the machine.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "measurement; run with --ignored --nocapture"]
async fn warm_exact_target_input_stays_below_the_interactive_p95_budget() {
    let mut latencies = warm_exact_target_input_latencies().await;
    latencies.sort_unstable();
    let p95 = latencies[latencies.len().saturating_mul(95) / 100];
    eprintln!("warm exact-target Iroh input p95: {p95:?}");
    assert!(
        p95 < Duration::from_millis(50),
        "warm authenticated input p95 must stay below 50 ms, got {p95:?}; samples={latencies:?}"
    );
}

/// Publish to receipt of 100 warm exact-target inputs, one at a time.
async fn warm_exact_target_input_latencies() -> Vec<Duration> {
    const SURFACE: u64 = 77;
    const SAMPLES: usize = 100;

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
    latencies
}

fn percentile(sorted: &[Duration], pct: usize) -> Duration {
    sorted[(sorted.len().saturating_mul(pct) / 100).min(sorted.len().saturating_sub(1))]
}

fn report(label: &str, mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    let (p50, p95, max) = (
        percentile(&samples, 50),
        percentile(&samples, 95),
        *samples.last().expect("samples"),
    );
    eprintln!(
        "{label}: p50 {p50:?}, p95 {p95:?}, max {max:?} over {} samples",
        samples.len()
    );
    p95
}

/// The tab ids of `room`'s first window group, in order, as `tree` holds them.
fn group_tabs(tree: &WorkspaceTree, room: RoomId) -> Vec<u64> {
    tree.desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .and_then(|workspace| workspace.strips.first())
        .and_then(|strip| strip.window_groups.first())
        .map(|group| group.tabs.iter().map(|tab| tab.id).collect())
        .unwrap_or_default()
}

fn notes_tab(id: u64) -> Tab {
    Tab {
        id,
        title: format!("Notes {id:x}"),
        tile: TileContent::Stub {
            kind: AppKind::Other("so.tilo.notes".into()),
            label: "Notes".into(),
        },
        app_title: None,
    }
}

fn room_push_tree(room: RoomId, salt: u16) -> WorkspaceTree {
    let id = |counter: u64| compose_id(salt, counter);
    let mut a = WorkspaceTree::default();
    a.set_salt_override(salt);
    let mut workspace = Workspace::in_room(WorkspaceId(id(1)), room, "Live");
    workspace.strips.push(Strip {
        id: StripId(id(2)),
        name: "Main".into(),
        window_groups: vec![WindowGroup {
            id: id(3),
            tabs: vec![notes_tab(id(100))],
            active_tab: id(100),
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    });
    a.desktop.active = workspace.id;
    a.desktop.workspaces.push(workspace);
    a.seed_doc_from_cache();

    a
}

fn report_tab_push_work(samples: usize, work: Vec<(usize, usize)>) {
    let (batches, changes) = work.into_iter().fold((0_usize, 0_usize), |sum, counts| {
        (
            sum.0.checked_add(counts.0).expect("batch total fits"),
            sum.1.checked_add(counts.1).expect("change total fits"),
        )
    });
    eprintln!("pushed-tab-work samples={samples} batches={batches} changes={changes}");
}

/// Two devices share one room over real Iroh. Device A opens, moves and
/// closes tabs; each change is pushed the way the shell pushes it (the room
/// document's changes since what A last announced, as one `SyncResp`). Each
/// mutation must converge with one incremental change in one received batch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pushed_tab_change_reaches_the_other_device_in_one_incremental_batch() {
    const SALT_A: u16 = 0xA0A0;
    const SALT_B: u16 = 0xB0B0;
    const SAMPLES: usize = 100;

    let (cmd_a, mut events_a, addr_a) = spawn_actor().await;
    let (cmd_b, mut events_b, addr_b) = spawn_actor().await;
    let a_id = addr_a.id;
    connect_pair(&cmd_a, addr_a, &cmd_b, addr_b).await;
    wait_peer_joined(&mut events_a).await;
    wait_peer_joined(&mut events_b).await;

    let room = RoomId::new();
    let id = |counter: u64| compose_id(SALT_A, counter);
    let mut a = room_push_tree(room, SALT_A);

    let mut b = WorkspaceTree::default();
    b.set_salt_override(SALT_B);
    b.join_room("Placeholder", room).expect("joins the room");
    // The invite names A's endpoint as the room's governor.
    assert!(b.pin_room_authority(room, PeerOrigin::peer(&a_id)));

    let publish = |changes: Vec<Vec<u8>>| NetCmd::Publish {
        ws: WS,
        msg: NeighbourMsg::SyncResp {
            changes,
            room: room.to_string(),
        },
    };
    let mut receive = async |b: &mut WorkspaceTree, want: &[u64]| {
        let mut batches = 0;
        let mut applied_changes = 0;
        while group_tabs(b, room) != want {
            let event = events_b.recv().await.expect("B's events");
            if let NetEvent::Msg {
                peer,
                msg:
                    NeighbourMsg::SyncResp {
                        changes,
                        room: received_room,
                    },
                ..
            } = event
            {
                assert_eq!(received_room, room.to_string());
                batches += 1;
                applied_changes += changes.len();
                b.apply_room_changes(room, &changes, &PeerOrigin::peer(&peer))
                    .expect("B applies A's push");
            }
        }
        (batches, applied_changes)
    };

    // Warm-up: B joins with A's whole room, as its first pull would bring.
    let baseline = a.export_room_changes_since(room, &[]).expect("A's room");
    let mut announced = a.room_heads(room);
    cmd_a
        .send(publish(baseline))
        .await
        .expect("baseline queued");
    let want = group_tabs(&a, room);
    tilo_test_env::within("B joined A's room", receive(&mut b, &want)).await;

    let mut next = 101;
    // The companion ShellNetwork next-tick test checks the scheduling boundary;
    // this real transport test checks the work required to converge each edit.
    let mut work = Vec::with_capacity(SAMPLES);
    for sample in 0..SAMPLES {
        {
            let workspace = &mut a.desktop.workspaces[0];
            let tabs = &mut workspace.strips[0].window_groups[0].tabs;
            match sample % 3 {
                // Open a tab.
                0 => {
                    tabs.push(notes_tab(id(next)));
                    next = next.saturating_add(1);
                }
                // Move the last tab to the front.
                1 => {
                    let moved = tabs.pop().expect("a tab");
                    tabs.insert(0, moved);
                }
                // Close the first tab, keeping one open.
                _ if tabs.len() > 1 => {
                    tabs.remove(0);
                }
                _ => tabs.push(notes_tab(id(next))),
            }
        }
        a.seed_doc_from_cache();
        let changes = a
            .export_room_changes_since(room, &announced)
            .expect("A's room");
        announced = a.room_heads(room);
        assert_eq!(
            changes.len(),
            1,
            "sample {sample} exports one incremental change"
        );
        cmd_a.send(publish(changes)).await.expect("push queued");
        let want = group_tabs(&a, room);
        let counts = tilo_test_env::within("B shows A's change", receive(&mut b, &want)).await;
        assert_eq!(
            counts,
            (1, 1),
            "sample {sample} converges in one batch and one change"
        );
        work.push(counts);
    }
    report_tab_push_work(SAMPLES, work);
}

/// A document edit travels as one signed `AppDocument` on the room topic and
/// reaches the other device, each before the next is published.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_document_edit_reaches_the_other_device() {
    let latencies = document_edit_latencies().await;
    assert_eq!(latencies.len(), 100, "every edit arrived");
}

/// The same edits arrive in under 50 ms at p95. Wall-clock time depends on
/// the machine.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "measurement; run with --ignored --nocapture"]
async fn a_document_edit_reaches_the_other_device_within_the_interactive_budget() {
    let p95 = report(
        "document edit, A publish -> B receive (loopback Iroh)",
        document_edit_latencies().await,
    );
    assert!(
        p95 < Duration::from_millis(50),
        "a document edit must reach the other device in under 50 ms at p95, got {p95:?}"
    );
}

/// Publish to receipt of 100 keystroke-sized edits, one at a time.
async fn document_edit_latencies() -> Vec<Duration> {
    const SAMPLES: usize = 100;
    let (cmd_a, mut events_a, addr_a) = spawn_actor().await;
    let (cmd_b, mut events_b, addr_b) = spawn_actor().await;
    connect_pair(&cmd_a, addr_a, &cmd_b, addr_b).await;
    wait_peer_joined(&mut events_a).await;
    wait_peer_joined(&mut events_b).await;

    let edit = |sequence: u64| {
        // A typical keystroke-sized Automerge change.
        let mut doc = automerge::AutoCommit::new();
        let _ = automerge::transaction::Transactable::put(
            &mut doc,
            automerge::ROOT,
            "text",
            format!("edit {sequence}"),
        );
        NetCmd::Publish {
            ws: WS,
            msg: NeighbourMsg::AppDocument {
                peer: 7,
                app_id: "so.tilo.notes".into(),
                cluster_id: RoomId::new().to_string(),
                record_id: "uuid:notes".into(),
                envelope: Box::new(collab_types::SignedOp::new_unsigned(
                    doc.save(),
                    collab_types::ArtifactId(uuid::Uuid::from_bytes([0x11; 16])),
                    collab_types::Ed25519PublicKey::from_bytes([7; 32]),
                    collab_types::AttestationId(uuid::Uuid::from_bytes([0x22; 16])),
                    collab_types::OpKind::Change,
                    std::time::SystemTime::now(),
                    sequence,
                )),
            },
        }
    };
    let arrived = async |events: &mut mpsc::Receiver<NetEvent>, sequence: u64| loop {
        if let Some(NetEvent::Msg {
            msg: NeighbourMsg::AppDocument { envelope, .. },
            ..
        }) = events.recv().await
            && envelope.seq == sequence
        {
            return;
        }
    };
    cmd_a.send(edit(u64::MAX)).await.expect("warm edit queued");
    tilo_test_env::within("warm edit arrived", arrived(&mut events_b, u64::MAX)).await;
    let mut latencies = Vec::with_capacity(SAMPLES);
    for sequence in 0..SAMPLES as u64 {
        let started = Instant::now();
        cmd_a.send(edit(sequence)).await.expect("edit queued");
        tilo_test_env::within("edit arrived", arrived(&mut events_b, sequence)).await;
        latencies.push(started.elapsed());
    }
    latencies
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
    // A remote-action frame sent after it is the marker: the heartbeat, had
    // the route carried it, would have arrived first.
    let marker = b"{\"version\":2}".to_vec();
    cmd_a
        .send(NetCmd::PublishRemoteAction {
            ws: WS,
            target,
            msg: NeighbourMsg::RemoteAction {
                frame: marker.clone(),
            },
        })
        .await
        .expect("marker queued");
    tilo_test_env::within("the marker frame arrived", async {
        loop {
            match events_b.recv().await {
                Some(NetEvent::Msg {
                    msg: NeighbourMsg::RemoteAction { frame },
                    ..
                }) if frame == marker => return,
                Some(NetEvent::Msg {
                    msg: NeighbourMsg::Heartbeat { peer: 1, ts: 1 },
                    ..
                }) => panic!("only remote-action frames use this route"),
                Some(_) => {}
                None => panic!("event channel closed"),
            }
        }
    })
    .await;
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
    device_label: u8,
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
    // Parallel CI jobs must not claim the same Iroh device identity on N0's
    // shared relays. Keep the fixture's person, but give each device its own key.
    let transport_key = iroh::SecretKey::generate();
    let key = ed25519_dalek::SigningKey::from_bytes(&transport_key.to_bytes());
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("Synthetic device {device_label}"),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    let lookup = MemoryLookup::new();
    let endpoint = iroh::Endpoint::builder(presets::N0)
        .secret_key(transport_key)
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
            session: native_fabric::OwnerSession::default(),
            stamp: stamp.clone(),
        },
        display: Some(native_fabric::DisplayPlacement {
            outputs: vec![native_fabric::DisplayConnectorPlacement {
                connector: "eDP-1".into(),
                label: "Synthetic panel".into(),
                rect: native_fabric::DisplayRect { x: 0, y: 0, ..rect },
                scale_milli,
            }],
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
fn native_pair_fabrics(
    source: &DeviceId,
    target: &DeviceId,
) -> (native_fabric::DeviceFabric, native_fabric::DeviceFabric) {
    let mut source_fabric = native_fabric::DeviceFabric::default();
    let mut target_fabric = native_fabric::DeviceFabric::default();
    source_fabric
        .upsert(native_snapshot(
            source,
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
            target,
            1000,
            native_fabric::DisplayRect {
                x: 960,
                y: 0,
                width: 1280,
                height: 720,
            },
        ))
        .unwrap();
    (source_fabric, target_fabric)
}

async fn send_native_connector_observation(
    command: &tokio::sync::mpsc::Sender<NetCmd>,
    target: iroh::EndpointId,
    events: &mut tokio::sync::mpsc::Receiver<NetEvent>,
    source: iroh::EndpointId,
    fabrics: (
        &mut native_fabric::DeviceFabric,
        &mut native_fabric::DeviceFabric,
    ),
    owner: &DeviceId,
) {
    let display = fabrics
        .0
        .snapshots()
        .get(owner)
        .unwrap()
        .display
        .as_ref()
        .unwrap()
        .clone();
    send_native_payload(
        command,
        target,
        PersonalFabricPayload::DisplayConnectors {
            stamp: display.stamp.clone(),
            outputs: display.outputs.clone(),
        },
    )
    .await;
    let PersonalFabricPayload::DisplayConnectors { stamp, outputs } =
        wait_native_payload(events, source).await
    else {
        panic!("expected native connector observation")
    };
    fabrics
        .1
        .observe_display_connectors(owner, &stamp, outputs)
        .unwrap();
    let rich_outputs: Vec<_> = display
        .outputs
        .iter()
        .map(|connector| native_fabric::DisplayOutputObservation {
            connector: connector.connector.clone(),
            output: settings_store::person_displays::DeviceOutput {
                id: format!("monitor:{}", connector.connector),
                name: connector.label.clone(),
                builtin: false,
                x: connector.rect.x,
                y: connector.rect.y,
                width: connector.rect.width,
                height: connector.rect.height,
                mode_width: connector.rect.width,
                mode_height: connector.rect.height,
                refresh_mhz: 60_000,
                scale_milli: connector.scale_milli,
                connected: true,
                enabled: true,
            },
        })
        .collect();
    fabrics
        .0
        .observe_display_outputs(owner, &display.stamp, rich_outputs.clone())
        .unwrap();
    send_native_payload(
        command,
        target,
        PersonalFabricPayload::DisplayOutputs {
            stamp: display.stamp.clone(),
            outputs: rich_outputs.clone(),
        },
    )
    .await;
    let PersonalFabricPayload::DisplayOutputs { stamp, outputs } =
        wait_native_payload(events, source).await
    else {
        panic!("expected rich native output observation")
    };
    fabrics
        .1
        .observe_display_outputs(owner, &stamp, outputs)
        .unwrap();
    assert_eq!(
        fabrics.1.display_outputs(owner),
        Some(rich_outputs.as_slice())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_screen_crossing_reaches_the_two_peer_seat_without_video() {
    let (cmd_a, mut events_a, addr_a) = spawn_same_identity_actor(42).await;
    let (cmd_b, mut events_b, addr_b) = spawn_same_identity_actor(42).await;
    assert_ne!(
        addr_a.id, addr_b.id,
        "equal device labels must retain distinct authenticated endpoints"
    );
    let source = DeviceId::new(hex::encode(addr_a.id.as_bytes())).unwrap();
    let target = DeviceId::new(hex::encode(addr_b.id.as_bytes())).unwrap();
    let endpoint_a = addr_a.id;
    let endpoint_b = addr_b.id;
    connect_pair(&cmd_a, addr_a, &cmd_b, addr_b).await;
    wait_certified_native_peer(&mut events_a).await;
    wait_certified_native_peer(&mut events_b).await;
    let (mut source_fabric, mut target_fabric) = native_pair_fabrics(&source, &target);
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
    send_native_connector_observation(
        &cmd_b,
        endpoint_a,
        &mut events_a,
        endpoint_b,
        (&mut target_fabric, &mut source_fabric),
        &target,
    )
    .await;
    let remote = source_fabric.snapshots().get(&target).unwrap().clone();
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
    send_native_connector_observation(
        &cmd_a,
        endpoint_b,
        &mut events_b,
        endpoint_a,
        (&mut source_fabric, &mut target_fabric),
        &source,
    )
    .await;
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
    use shell::shell::input_policy::InputCapture;
    use smithay::reexports::calloop::EventLoop;
    use smithay::reexports::wayland_server::Display;
    let event_loop: EventLoop<'static, State> = EventLoop::try_new().unwrap();
    let display: Display<State> = Display::new().unwrap();
    let mut state = State::new(event_loop.handle(), display.handle());
    // Admitted input is delivered by the event loop as this device's own.
    assert!(state.apply_personal_fabric_input(controller, event));
    assert_eq!(state.take_personal_fabric_input(), vec![event]);
    let key = |pressed| native_fabric::FabricInputEvent::Key {
        keycode: 30,
        pressed,
        modifiers: 0,
    };
    assert!(state.apply_personal_fabric_input(controller, key(true)));
    assert_eq!(state.take_personal_fabric_input(), vec![key(true)]);
    assert!(state.release_personal_fabric_input(controller));
    assert_eq!(
        state.take_personal_fabric_input(),
        vec![key(false)],
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
        .publish_view(ShellView::new(Vec::new(), None, InputCapture::NONE, false).locked(true));
    assert!(!state.apply_personal_fabric_input(controller, event));
    state.remote_input_firewall.publish_view(ShellView::new(
        Vec::new(),
        None,
        InputCapture::NONE,
        false,
    ));
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

#[test]
fn native_connector_discovery_preserves_owner_scale_and_offline_state() {
    use shell::shell::device_fabric_runtime::observed_device_displays;
    let source = DeviceId::new("source").unwrap();
    let target = DeviceId::new("target").unwrap();
    let mut fabric = native_fabric::DeviceFabric::default();
    let rect = native_fabric::DisplayRect {
        x: 0,
        y: 0,
        width: 960,
        height: 540,
    };
    fabric.upsert(native_snapshot(&source, 2000, rect)).unwrap();
    let mut peer = native_snapshot(&target, 1000, native_fabric::DisplayRect { x: 960, ..rect });
    peer.display.as_mut().unwrap().rect.width = 1920;
    peer.display
        .as_mut()
        .unwrap()
        .outputs
        .push(native_fabric::DisplayConnectorPlacement {
            connector: "DP-1".into(),
            label: "Second panel".into(),
            scale_milli: 1250,
            rect: native_fabric::DisplayRect { x: 960, ..rect },
        });
    fabric.upsert(peer.clone()).unwrap();
    let observed = observed_device_displays(&fabric, Some(&source));
    assert_eq!(observed.len(), 3);
    assert_ne!(
        observed[0].key(),
        observed[1].key(),
        "matching eDP names retain exact device identity"
    );
    assert_eq!(
        (observed[0].scale_milli, observed[1].scale_milli),
        (2000, 1000)
    );
    assert_eq!(
        (observed[2].connector.as_str(), observed[2].x),
        ("DP-1", 1920)
    );
    peer.presence.online = false;
    peer.presence.stamp.counter += 1;
    fabric.upsert(peer.clone()).unwrap();
    let offline = observed_device_displays(&fabric, Some(&source));
    assert!(offline[0].online && offline[0].local);
    assert!(
        offline[1..]
            .iter()
            .all(|display| !display.online && !display.local)
    );
    peer.display.as_mut().unwrap().outputs[1].connector = "eDP-1".into();
    assert!(
        peer.validate().is_err(),
        "duplicate connectors cannot enter authenticated topology"
    );
    peer.display.as_mut().unwrap().outputs[1].connector = "DP-1".into();
    peer.display.as_mut().unwrap().outputs[1].rect.x = i32::MAX;
    assert!(
        peer.validate().is_err(),
        "overflowing connector bounds fail closed"
    );
}

#[test]
fn native_crossing_uses_connector_edges_and_destination_seat_coordinates() {
    let source = DeviceId::new("source").unwrap();
    let target = DeviceId::new("target").unwrap();
    let mut fabric = native_fabric::DeviceFabric::default();
    let source_rect = native_fabric::DisplayRect {
        x: 0,
        y: 0,
        width: 960,
        height: 1080,
    };
    fabric
        .upsert(native_snapshot(&source, 2000, source_rect))
        .unwrap();
    let mut peer = native_snapshot(
        &target,
        1000,
        native_fabric::DisplayRect {
            x: 960,
            width: 1920,
            ..source_rect
        },
    );
    peer.display.as_mut().unwrap().outputs = vec![
        native_fabric::DisplayConnectorPlacement {
            connector: "upper".into(),
            label: "Upper screen".into(),
            scale_milli: 1000,
            rect: native_fabric::DisplayRect {
                x: 0,
                y: 0,
                width: 960,
                height: 540,
            },
        },
        native_fabric::DisplayConnectorPlacement {
            connector: "lower".into(),
            label: "Lower screen".into(),
            scale_milli: 1250,
            rect: native_fabric::DisplayRect {
                x: 0,
                y: 540,
                width: 960,
                height: 540,
            },
        },
    ];
    fabric.upsert(peer.clone()).unwrap();
    let crossing = fabric.cross_right_edge(&source, 700).unwrap();
    assert_eq!(
        (crossing.to, crossing.target_x, crossing.target_y),
        (target, 0, 700),
        "the lower connector receives its actual seat coordinate, not y=160"
    );
    peer.presence.stamp.counter += 1;
    peer.display.as_mut().unwrap().outputs[1].rect.x = 960;
    fabric.upsert(peer).unwrap();
    assert!(
        fabric.cross_right_edge(&source, 700).is_none(),
        "a device bounding box cannot bridge the empty space between connectors"
    );
}

#[test]
fn native_crossing_preserves_negative_connector_coordinates() {
    let source = DeviceId::new("source").unwrap();
    let target = DeviceId::new("target").unwrap();
    let rect = native_fabric::DisplayRect {
        x: 0,
        y: 0,
        width: 960,
        height: 540,
    };
    let mut own = native_snapshot(&source, 2000, rect);
    own.display.as_mut().unwrap().outputs[0].rect.x = -960;
    let mut peer = native_snapshot(
        &target,
        1250,
        native_fabric::DisplayRect { x: -1920, ..rect },
    );
    peer.display.as_mut().unwrap().outputs[0].rect.y = -100;
    let mut fabric = native_fabric::DeviceFabric::default();
    fabric.upsert(own).unwrap();
    fabric.upsert(peer).unwrap();
    let crossing = fabric
        .cross_edge(&source, native_fabric::DisplayEdge::Left, -960, 205)
        .unwrap();
    assert_eq!(
        (crossing.to, crossing.target_x, crossing.target_y),
        (target, 959, 205)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_checked_placement_is_transport_bound_and_stale_after_owner_relayout() {
    let (cmd_a, mut events_a, addr_a) = spawn_same_identity_actor(46).await;
    let (cmd_b, mut events_b, addr_b) = spawn_same_identity_actor(47).await;
    let source = DeviceId::new(hex::encode(addr_a.id.as_bytes())).unwrap();
    let target = DeviceId::new(hex::encode(addr_b.id.as_bytes())).unwrap();
    let endpoint_a = addr_a.id;
    let endpoint_b = addr_b.id;
    connect_pair(&cmd_a, addr_a, &cmd_b, addr_b).await;
    wait_certified_native_peer(&mut events_a).await;
    wait_certified_native_peer(&mut events_b).await;
    let (_, owner) = native_pair_fabrics(&source, &target);
    let mut before = owner.snapshots().get(&target).unwrap().clone();
    let display = before.display.as_ref().unwrap();
    let rect = native_fabric::DisplayRect {
        x: 0,
        y: 540,
        ..display.rect
    };
    let expected = display.stamp.clone();
    let now = tilo_clock::unix_now_ms();
    let envelope = native_fabric::PersonalFabricActionEnvelope {
        request_id: [17; 16],
        source: source.clone(),
        target: target.clone(),
        issued_unix_ms: now,
        action: native_fabric::PersonalFabricAction::SetDisplayPlacementChecked {
            expected: expected.clone(),
            rect,
            expires_unix_ms: now.saturating_add(native_fabric::MAX_DISPLAY_MOVE_AGE_MS),
        },
    };
    let received =
        send_native_action(&cmd_a, &mut events_b, (endpoint_a, endpoint_b), &envelope).await;
    received.validate_at(&source, &target, now).unwrap();
    assert!(received.validate_at(&target, &source, now).is_err());
    assert!(
        received
            .validate_at(
                &source,
                &target,
                now.saturating_add(native_fabric::MAX_DISPLAY_MOVE_AGE_MS + 1)
            )
            .is_err(),
        "delayed gestures expire before applying after reconnect"
    );
    let native_fabric::PersonalFabricAction::SetDisplayPlacementChecked { expected, rect, .. } =
        received.action
    else {
        panic!("checked placement");
    };
    assert!(
        before
            .display
            .as_ref()
            .unwrap()
            .accepts_move(&expected, rect)
    );
    assert_topology_grants_no_keyboard_input(&source, &target, now);
    before.presence.stamp.counter = 2;
    let authored = before.display.as_mut().unwrap();
    authored.stamp.counter = 2;
    authored.rect = rect;
    let mut published = native_fabric::DeviceFabric::default();
    published.upsert(before).unwrap();
    send_native_payload(
        &cmd_b,
        endpoint_a,
        PersonalFabricPayload::Snapshot(published.encode_checkpoint().unwrap()),
    )
    .await;
    let PersonalFabricPayload::Snapshot(bytes) =
        wait_native_payload(&mut events_a, endpoint_b).await
    else {
        panic!("owner-authored placement checkpoint");
    };
    let confirmed = native_fabric::DeviceFabric::decode_checkpoint(&bytes).unwrap();
    let actual = confirmed
        .snapshots()
        .get(&target)
        .unwrap()
        .display
        .as_ref()
        .unwrap();
    assert_eq!(actual.rect, rect);
    assert_eq!(actual.stamp.counter, 2);
    assert!(
        !actual.accepts_move(&expected, rect),
        "replaying the previous drag cannot replace a newer owner layout"
    );
}

fn assert_topology_grants_no_keyboard_input(source: &DeviceId, target: &DeviceId, now: u64) {
    assert!(
        !native_fabric::permits_spatial_input(
            None,
            source,
            target,
            (
                [9; 16],
                native_fabric::FabricInputEvent::Key {
                    keycode: 30,
                    pressed: true,
                    modifiers: 0
                }
            ),
            now
        ),
        "certified topology control grants no keyboard input"
    );
}

async fn send_native_action(
    cmd: &mpsc::Sender<NetCmd>,
    events: &mut mpsc::Receiver<NetEvent>,
    endpoints: (iroh::EndpointId, iroh::EndpointId),
    envelope: &native_fabric::PersonalFabricActionEnvelope,
) -> native_fabric::PersonalFabricActionEnvelope {
    send_native_payload(
        cmd,
        endpoints.1,
        PersonalFabricPayload::Action(postcard::to_allocvec(envelope).unwrap()),
    )
    .await;
    let PersonalFabricPayload::Action(bytes) = wait_native_payload(events, endpoints.0).await
    else {
        panic!("typed placement action");
    };
    postcard::from_bytes(&bytes).unwrap()
}
